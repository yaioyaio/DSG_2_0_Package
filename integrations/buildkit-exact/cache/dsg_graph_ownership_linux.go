//go:build linux

package cache

import (
	"bytes"
	"errors"
	"strconv"
	"strings"

	"github.com/containerd/containerd/v2/core/snapshots"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

// DSGRetentionOwnership describes the exact retention relationships in the
// observed transactions. It grants no authority to modify a store.
type DSGRetentionOwnership struct {
	Checked                  bool   `json:"checked"`
	Exclusive                bool   `json:"exclusive"`
	RecordLease              bool   `json:"record_lease"`
	VariantLease             bool   `json:"variant_lease"`
	ViewLease                bool   `json:"view_lease"`
	OwnedRoots               int    `json:"owned_roots"`
	OwnedChildSnapshots      int    `json:"owned_child_snapshots"`
	IncomingContentKeySHA256 string `json:"incoming_content_key_sha256,omitempty"`
	Reason                   string `json:"reason"`
}

func (r *dsgGraphReader) retentionOwnership(o *DSGDedicatedGraph, cache, backend, native *bolt.Tx, fd int, id, snapshot, blob string) *DSGRetentionOwnership {
	out := &DSGRetentionOwnership{Reason: "incomplete_graph"}
	if r.gcWalk == nil || o.Cache == nil || o.External == nil || o.Solver == nil || o.Containerd == nil ||
		o.Snapshot == nil || o.Transitive == nil || o.Transitive.CacheRecords == nil || o.Transitive.Solver == nil || o.Transitive.GCLabels == nil ||
		cache == nil || backend == nil || native == nil || !o.Transitive.GCLabels.HistoryChecked {
		return out
	}
	for _, code := range o.Unverified {
		if code != "native_allocation_contents" {
			return out
		}
	}
	out.Checked = true
	c, b := o.Cache, o.Containerd
	if !c.Committed || c.Deleted || c.BlobOnly || c.Parents != 0 || c.Children != 0 || c.AliasesIn != 0 || c.AliasesOut != 0 ||
		c.SnapshotPeers != 0 || c.BlobPeers != 0 || o.Transitive.CacheRecords.Records != 1 || c.ExternalEntries != 0 ||
		o.Solver.ParentLinks != 0 || o.Solver.ChildLinks != 0 || o.Solver.ResultNodes > 1 || r.solverSharedResult {
		out.Reason = "shared_or_unsupported_cache_record"
		return out
	}
	if !b.SnapshotMapping || b.NativeSnapshotAliases != 0 || b.BlobNamespaces != 1 || blob == "" ||
		o.Snapshot.Kind != snapshots.KindCommitted.String() || !o.Snapshot.DirectoryPresent || o.Snapshot.Parents != 0 ||
		o.Allocations == nil || o.Allocations.Snapshot == nil || o.Allocations.Blob == nil || o.Blob == nil {
		out.Reason = "shared_or_missing_allocation"
		return out
	}
	if !b.RecordLease || !b.TargetLeaseSnapshot || !b.TargetLeaseBlob || b.RecordLeaseSnapshots != 1 ||
		b.RecordLeaseBlobs != 1 || b.OtherOwnedResources != 0 || b.VariantLeaseSnapshots != 0 ||
		(b.VariantLease && b.VariantLeaseBlobs != 1) {
		out.Reason = "lease_resources_not_exact"
		return out
	}
	view := snapshot + "-view"
	if b.SnapshotChildren != o.Snapshot.Children || b.SnapshotChildren > 1 ||
		(b.ViewLease && b.SnapshotChildren != 1) || (b.ViewLease && (b.ViewLeaseBlobs != 0 || b.ViewLeaseSnapshots != 1)) {
		out.Reason = "child_or_view_lease_not_exact"
		return out
	}
	hasView := b.SnapshotChildren == 1
	root := backend.Bucket([]byte("v1"))
	if !r.verifySnapshotIndexes(root, native, cache, fd, snapshot, view, hasView) {
		out.Reason = "snapshot_indexes_or_allocation_not_exact"
		return out
	}
	ns := root.Bucket([]byte(dsgGraphNamespace))
	leases := ns.Bucket([]byte("leases"))
	for _, role := range []struct {
		id                 string
		present, temporary bool
	}{
		{id, true, false}, {id + "-variants", b.VariantLease, false}, {id + "-view", b.ViewLease, true},
	} {
		if !role.present {
			continue
		}
		lease := leases.Bucket([]byte(role.id))
		labels, err := dsgGraphBucket(lease, "labels")
		if lease == nil || err != nil || r.each(labels, func(k, v []byte) error {
			if v == nil || (string(k) != "containerd.io/gc.flat" && !(role.temporary && string(k) == "buildkit/lease.temporary")) {
				return ErrDSGExactPreserve
			}
			return nil
		}) != nil {
			out.Reason = "unrecognized_lease_metadata"
			return out
		}
	}
	content := ns.Bucket([]byte("content")).Bucket([]byte("blob")).Bucket([]byte(blob))
	labels, err := dsgGraphBucket(content, "labels")
	if err != nil {
		out.Reason = "unrecognized_content_metadata"
		return out
	}
	self := labels != nil && bytes.Equal(labels.Get([]byte(blobVariantGCLabel+blob)), []byte(blob))
	r.gcWalk.classifyRetentionEdges(out, id, snapshot, blob, b.VariantLease, hasView, b.ViewLease, self)
	if !out.Exclusive {
		out.Reason = "foreign_or_unresolved_retention_edge"
		return out
	}
	out.Reason = "exact_record_resources"
	return out
}

func (w *dsgGCWalk) classifyRetentionEdges(out *DSGRetentionOwnership, id, snapshot, blob string, variants, view, viewLease, self bool) {
	ns := dsgOpaqueKey([]byte(dsgGraphNamespace))
	node := func(kind, id string) dsgGCNode { return dsgGCNode{ns, kind, dsgOpaqueKey([]byte(id))} }
	s, c, lease := node("snapshot", snapshot), node("content", blob), node("lease", id)
	roots := map[dsgGCNode]bool{lease: true, s: false, c: false}
	type edge struct{ from, to dsgGCNode }
	edges := map[edge]int{{lease, s}: 1, {lease, c}: 1}
	if self {
		edges[edge{c, c}] = 1
	}
	if variants {
		v := node("lease", id+"-variants")
		roots[v] = true
		edges[edge{v, c}] = 1
	}
	if view {
		v, child := node("lease", id+"-view"), node("snapshot", snapshot+"-view")
		roots[child] = false
		edges[edge{child, s}] = 1
		if viewLease {
			roots[v] = true
			edges[edge{v, child}] = 1
		}
	}
	// Check each participant's incoming and outgoing adjacency independently.
	// This retains positive identities when an otherwise unrelated manifest
	// points at the blob, without reporting the entire closure as exclusive.
	exactNeighbors := func(n dsgGCNode, adjacent []dsgGCNode, forward bool) bool {
		expected := make(map[dsgGCNode]int)
		for e, count := range edges {
			if forward && e.from == n {
				expected[e.to] = count
			}
			if !forward && e.to == n {
				expected[e.from] = count
			}
		}
		if len(adjacent) != len(expected) {
			return false
		} // all allowed multiplicities are one
		for _, other := range adjacent {
			if expected[other] != 1 {
				return false
			}
			delete(expected, other)
		}
		return len(expected) == 0
	}
	exact := func(n dsgGCNode) bool {
		return w.nodes[n] && w.roots[n] == roots[n] &&
			exactNeighbors(n, w.paths.forward[n], true) && exactNeighbors(n, w.paths.reverse[n], false)
	}
	// Publish only a fixed-width fingerprint when exactly one existing local
	// content object points to this digest. Never expose arbitrary object names.
	var source string
	for _, from := range w.paths.reverse[c] {
		if from == c || from.kind != "content" {
			continue
		}
		if !w.nodes[from] || from.ns != ns || (source != "" && from.key != source) {
			source = ""
			break
		}
		source = from.key
	}
	out.IncomingContentKeySHA256 = source
	out.RecordLease = exact(lease)
	out.VariantLease = variants && exact(node("lease", id+"-variants"))
	out.ViewLease = viewLease && exact(node("lease", id+"-view"))
	for _, retained := range []bool{out.RecordLease, out.VariantLease, out.ViewLease} {
		if retained {
			out.OwnedRoots++
		}
	}
	if view && exact(node("snapshot", snapshot+"-view")) && (!viewLease || out.ViewLease) {
		out.OwnedChildSnapshots = 1
	}
	for _, seed := range w.seeds {
		if seed != s && seed != c {
			return
		}
	}
	out.Exclusive = out.RecordLease && (!variants || out.VariantLease) &&
		(!view || out.OwnedChildSnapshots == 1) && exact(s) && exact(c)
}

// Validate containerd's children index in both directions, not just the
// forward parent counts. The one supported child is BuildKit's immutable
// read-only view: both metadata stores must agree and it has no private data.
func (r *dsgGraphReader) verifySnapshotIndexes(root *bolt.Bucket, native, cache *bolt.Tx, fd int, snapshot, view string, hasView bool) bool {
	ns := root.Bucket([]byte(dsgGraphNamespace))
	mapping := dsgGraphNativeBucket(ns)
	if mapping == nil {
		return false
	}
	target := mapping.Bucket([]byte(snapshot))
	if target == nil {
		return false
	}
	backing := string(target.Get([]byte("name")))
	viewBacking := ""
	if hasView {
		child := mapping.Bucket([]byte(view))
		if child == nil || string(child.Get([]byte("parent"))) != snapshot {
			return false
		}
		viewBacking = string(child.Get([]byte("name")))
		if !dsgNativeKey(viewBacking) || !strings.HasSuffix(viewBacking, "/"+view) || viewBacking == backing {
			return false
		}
		var sub DSGDedicatedGraph
		if r.snapshot(&sub, native, fd, dsgGraphNativeKeys{key: viewBacking, parent: backing}) != nil ||
			sub.Snapshot.Kind != snapshots.KindView.String() || sub.Snapshot.Children != 0 {
			return false
		}
		nativeChild := native.Bucket([]byte("v1")).Bucket([]byte("snapshots")).Bucket([]byte(viewBacking))
		// mount creates a native view without labels. A released view may
		// outlive its temporary lease, but arbitrary labeled views are opaque.
		for _, row := range []*bolt.Bucket{child, nativeChild} {
			labels, err := dsgGraphBucket(row, "labels")
			if err != nil || (labels != nil && r.each(labels, func(_, _ []byte) error { return ErrDSGExactPreserve }) != nil) {
				return false
			}
		}
		rec, err := dsgGraphReadNativeSnapshot(nativeChild)
		if err != nil {
			return false
		}
		dir, err := dsgOpenOwned(fd, "snapshots", true)
		if err != nil {
			return false
		}
		defer dir.Close()
		// The allocation reader collapses all stat errors into preservation.
		// This negative lookup needs the kernel errno to distinguish absence.
		var stat unix.Statx_t
		err = unix.Statx(int(dir.Fd()), strconv.FormatUint(rec.id, 10), unix.AT_SYMLINK_NOFOLLOW|unix.AT_NO_AUTOMOUNT, unix.STATX_TYPE, &stat)
		if !errors.Is(err, unix.ENOENT) {
			return false
		}
		if err := r.each(cache.Bucket([]byte("_main")), func(k, v []byte) error {
			row, err := dsgReadGraphRecord(cache.Bucket([]byte("_main")).Bucket(k), string(k))
			if err != nil || row.snapshot == view {
				return ErrDSGExactPreserve
			}
			return nil
		}); err != nil {
			return false
		}
	}
	seen := false
	err := r.each(root, func(n, v []byte) error {
		if v != nil {
			return nil
		}
		b := dsgGraphNativeBucket(root.Bucket(n))
		worker := string(n) == dsgGraphNamespace
		return r.each(b, func(k, v []byte) error {
			if v != nil {
				return ErrDSGExactPreserve
			}
			row := b.Bucket(k)
			isTarget := worker && string(k) == snapshot
			isView := worker && hasView && string(k) == view
			if hasView && string(row.Get([]byte("name"))) == viewBacking && !isView {
				return ErrDSGExactPreserve
			}
			idx, err := dsgGraphBucket(row, "children")
			if err != nil {
				return err
			}
			return r.each(idx, func(child, value []byte) error {
				if value == nil || len(value) != 0 {
					return ErrDSGExactPreserve
				}
				if isTarget && hasView && string(child) == view {
					seen = true
					return nil
				}
				if isTarget || isView || (worker && (string(child) == snapshot || (hasView && string(child) == view))) {
					return ErrDSGExactPreserve
				}
				return nil
			})
		})
	})
	return err == nil && seen == hasView
}
