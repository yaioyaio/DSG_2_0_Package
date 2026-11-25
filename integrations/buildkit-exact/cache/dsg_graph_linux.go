//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"hash"
	"strings"

	"github.com/moby/buildkit/solver"
	bolt "go.etcd.io/bbolt"
)

const dsgGraphMaxEntries = 16384

// DSGDedicatedGraph describes direct references observed across pinned worker
// stores. Each checked section has an independent traversal and shared budget.
// The graph is evidence for preservation and carries no mutation authority.
type DSGDedicatedGraph struct {
	Cache        *DSGGraphCache       `json:"cache,omitempty"`
	Solver       *DSGGraphSolver      `json:"solver,omitempty"`
	Containerd   *DSGGraphBackend     `json:"containerd,omitempty"`
	Snapshot     *DSGGraphSnapshot    `json:"native_snapshot,omitempty"`
	Blob         *DSGGraphBlob        `json:"local_blob,omitempty"`
	Allocations  *DSGGraphAllocations `json:"native_allocations,omitempty"`
	Transitive   *DSGGraphTransitive  `json:"transitive,omitempty"`
	External     *DSGGraphExternal    `json:"external,omitempty"`
	EvidenceHash string               `json:"references_sha256"`
	Unverified   []string             `json:"unverified"`
	Disposition  string               `json:"disposition"`
}

type DSGGraphCache struct {
	Committed        bool `json:"committed"`
	Deleted          bool `json:"deleted"`
	BlobOnly         bool `json:"blob_only"`
	SnapshotFallback bool `json:"snapshot_id_fallback"`
	Parents          int  `json:"parents"`
	Children         int  `json:"children"`
	AliasesOut       int  `json:"aliases_out"`
	AliasesIn        int  `json:"aliases_in"`
	SnapshotPeers    int  `json:"snapshot_peers"`
	BlobPeers        int  `json:"blob_peers"`
	ExternalEntries  int  `json:"external_entries"`
}

type DSGGraphSolver struct {
	ResultNodes int `json:"result_nodes"`
	ParentLinks int `json:"parent_links"`
	ChildLinks  int `json:"child_links"`
}

type dsgGraphReader struct {
	ctx       context.Context
	remaining int
	evidence  hash.Hash
}

func (r *dsgGraphReader) edge(fields ...string) {
	// Parts are bounded identifiers selected from the pinned format, not
	// free-form metadata. JSON framing separates strings and edges.
	data, _ := json.Marshal(fields)
	r.evidence.Write(data)
	r.evidence.Write([]byte{'\n'})
}

func (r *dsgGraphReader) each(b *bolt.Bucket, visit func([]byte, []byte) error) error {
	if b == nil {
		return nil
	}
	return b.ForEach(func(k, v []byte) error {
		if len(k) > 1024 || !r.spend(1) {
			return ErrDSGExactPreserve
		}
		return visit(k, v)
	})
}

func (r *dsgGraphReader) spend(entries int) bool {
	if entries > r.remaining || r.ctx.Err() != nil {
		return false
	}
	r.remaining -= entries
	return true
}

func dsgGraphBucket(b *bolt.Bucket, key string) (*bolt.Bucket, error) {
	if b == nil {
		return nil, nil
	}
	if b.Get([]byte(key)) != nil {
		return nil, ErrDSGExactPreserve
	}
	return b.Bucket([]byte(key)), nil
}

func dsgGraphSHA256(value string) bool {
	encoded, ok := strings.CutPrefix(value, "sha256:")
	return ok && dsgDigest.MatchString(encoded)
}

func dsgGraphSnapshotKey(value string) bool {
	return dsgFullID.MatchString(value) || dsgGraphSHA256(value) ||
		(strings.HasSuffix(value, "-view") &&
			(dsgFullID.MatchString(strings.TrimSuffix(value, "-view")) ||
				dsgGraphSHA256(strings.TrimSuffix(value, "-view"))))
}

func dsgGraphSolverKey(value string) bool {
	encoded, random := strings.CutPrefix(value, "random:")
	return dsgFullID.MatchString(value) || dsgGraphSHA256(value) || (random && dsgDigest.MatchString(encoded))
}

func dsgObserveNativeGraph(ctx context.Context, workerID, recordID string, solverTx, cacheTx, containerdTx, snapshotTx *bolt.Tx, workerFD, snapshotterFD int, historyTxs ...*bolt.Tx) (*DSGDedicatedGraph, error) {
	if !dsgFullID.MatchString(workerID) || !dsgFullID.MatchString(recordID) || ctx.Err() != nil {
		return nil, ErrDSGExactPreserve
	}
	if len(historyTxs) > 1 {
		return nil, ErrDSGExactPreserve
	}
	var history *bolt.Tx
	if len(historyTxs) == 1 {
		history = historyTxs[0]
	}
	for _, tx := range []*bolt.Tx{solverTx, cacheTx, containerdTx, snapshotTx, history} {
		if tx != nil && tx.Writable() {
			return nil, ErrDSGExactPreserve
		}
	}

	o := &DSGDedicatedGraph{
		Disposition: "preserve",
		Transitive:  &DSGGraphTransitive{},
		Unverified:  []string{"native_allocation_contents"},
	}
	r := &dsgGraphReader{ctx: ctx, remaining: dsgGraphMaxEntries, evidence: sha256.New()}
	r.edge("dsg-native-reference-graph-v3", workerID, recordID)
	warn := func(code string) {
		o.Unverified = append(o.Unverified, code)
		r.edge("unverified", code)
	}

	snapshot, blob, err := r.cache(o, cacheTx, recordID)
	if err != nil {
		warn("cache_reference_projection")
	}
	if o.Transitive.CacheRecords == nil {
		warn("cache_record_transitive_parents_and_aliases")
	}
	if err := r.solver(o, solverTx, workerID+"::"+recordID); err != nil {
		warn("solver_result_or_link_indexes")
	}
	if o.Cache != nil {
		native, size, err := r.containerd(o, containerdTx, recordID, snapshot, blob)
		if err != nil {
			warn("containerd_lease_or_object_projection")
		} else {
			if native.key != "" {
				if err := r.snapshot(o, snapshotTx, snapshotterFD, native); err != nil {
					warn("native_snapshot_records_or_allocation")
				}
			} else {
				warn("native_snapshot_mapping_absent")
			}
			if blob != "" {
				if size == nil || r.blob(o, workerFD, blob, *size) != nil {
					warn("local_blob_metadata_or_allocation")
				}
			}
			r.allocations(o, snapshotTx, workerFD, snapshotterFD, native, blob, size, warn)
		}
	}
	r.extendedGraph(o, solverTx, cacheTx, containerdTx, history, workerID, recordID, snapshot, blob, warn)
	if ctx.Err() != nil {
		return nil, ErrDSGExactPreserve
	}
	// Bind the public record and completed counts as well as the selected edges.
	summary, err := json.Marshal(o)
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	r.evidence.Write(summary)
	o.EvidenceHash = hex.EncodeToString(r.evidence.Sum(nil))
	return o, nil
}

func dsgGraphValue[T any](b *bolt.Bucket, key string, dst *T) error {
	if b.Bucket([]byte(key)) != nil {
		return ErrDSGExactPreserve
	}
	raw := b.Get([]byte(key))
	if len(raw) == 0 {
		return nil
	}
	var parsed *T
	// The pinned writer marshals these fields through metadata.Value with no
	// Index. Decode the value only and leave any other properties opaque.
	payload, prefix := bytes.CutPrefix(raw, []byte(`{"value":`))
	payload, suffix := bytes.CutSuffix(payload, []byte("}"))
	if len(raw) > 4096 || !prefix || !suffix || json.Unmarshal(payload, &parsed) != nil || parsed == nil {
		return ErrDSGExactPreserve
	}
	*dst = *parsed
	return nil
}

type dsgGraphParent struct {
	role, id string
}

type dsgGraphRecord struct {
	snapshot, blob, alias string
	parents               []dsgGraphParent
	fallback              bool
}

func dsgReadGraphRecord(b *bolt.Bucket, id string) (dsgGraphRecord, error) {
	var a dsgGraphRecord
	for _, field := range []struct {
		key string
		dst *string
	}{
		{keySnapshot, &a.snapshot}, {keyBlob, &a.blob}, {keyEqualMutable, &a.alias},
	} {
		if err := dsgGraphValue(b, field.key, field.dst); err != nil {
			return a, err
		}
	}
	if a.snapshot == "" {
		// Mirrors cacheMetadata.getSnapshotID for historical records.
		a.snapshot, a.fallback = id, true
	}
	if !dsgGraphSnapshotKey(a.snapshot) || (a.blob != "" && !dsgGraphSHA256(a.blob)) ||
		(a.alias != "" && (!dsgFullID.MatchString(a.alias) || a.alias == id)) {
		return a, ErrDSGExactPreserve
	}

	for _, key := range []string{keyParent, keyLowerDiffParent, keyUpperDiffParent} {
		var parent string
		if err := dsgGraphValue(b, key, &parent); err != nil {
			return a, err
		}
		if parent != "" {
			a.parents = append(a.parents, dsgGraphParent{role: key, id: parent})
		}
	}
	var merged []string
	if err := dsgGraphValue(b, keyMergeParents, &merged); err != nil || len(merged) > 64 {
		return a, ErrDSGExactPreserve
	}
	// BuildKit's parentRefs is a disjoint union of layer, merge and diff
	// parents. Diff parents can have both a lower and an upper reference.
	if (len(a.parents) > 1 && a.parents[0].role == keyParent) || (len(a.parents) > 0 && len(merged) > 0) {
		return a, ErrDSGExactPreserve
	}
	for _, parent := range merged {
		a.parents = append(a.parents, dsgGraphParent{role: keyMergeParents, id: parent})
	}
	for _, parent := range a.parents {
		if !dsgFullID.MatchString(parent.id) || parent.id == id {
			return a, ErrDSGExactPreserve
		}
	}
	return a, nil
}

func (r *dsgGraphReader) cache(o *DSGDedicatedGraph, tx *bolt.Tx, id string) (string, string, error) {
	if tx == nil {
		return "", "", ErrDSGExactPreserve
	}
	main := tx.Bucket([]byte("_main"))
	if main == nil || main.Bucket([]byte(id)) == nil {
		return "", "", ErrDSGExactPreserve
	}
	b := main.Bucket([]byte(id))
	target, err := dsgReadGraphRecord(b, id)
	if err != nil {
		return "", "", err
	}
	out := &DSGGraphCache{SnapshotFallback: target.fallback, Parents: len(target.parents)}
	for _, field := range []struct {
		key string
		dst *bool
	}{
		{keyCommitted, &out.Committed}, {keyDeleted, &out.Deleted}, {keyBlobOnly, &out.BlobOnly},
	} {
		if err := dsgGraphValue(b, field.key, field.dst); err != nil {
			return "", "", err
		}
	}
	r.edge("cache-snapshot-blob", target.snapshot, target.blob)
	for _, parent := range target.parents {
		if main.Bucket([]byte(parent.id)) == nil {
			return "", "", ErrDSGExactPreserve
		}
		r.edge("cache-parent", parent.role, parent.id)
	}
	if target.alias != "" {
		if main.Bucket([]byte(target.alias)) == nil {
			return "", "", ErrDSGExactPreserve
		}
		out.AliasesOut++
		r.edge("cache-alias-out", target.alias)
	}
	graph := dsgCacheGraph{records: make(map[string]struct{})}
	err = r.each(main, func(k, v []byte) error {
		if v != nil || !dsgFullID.Match(k) {
			return ErrDSGExactPreserve
		}
		other := string(k)
		if other == id {
			return graph.add(r, id, target)
		}
		peer, err := dsgReadGraphRecord(main.Bucket(k), other)
		if err != nil {
			return err
		}
		for _, parent := range peer.parents {
			if parent.id == id {
				out.Children++
				r.edge("cache-child", parent.role, other)
			}
		}
		if peer.alias == id {
			out.AliasesIn++
			r.edge("cache-alias-in", other)
		}
		if peer.snapshot == target.snapshot {
			out.SnapshotPeers++
			r.edge("cache-snapshot-peer", other)
		}
		if target.blob != "" && peer.blob == target.blob {
			out.BlobPeers++
			r.edge("cache-blob-peer", other)
		}
		return graph.add(r, other, peer)
	})
	if err != nil {
		return "", "", err
	}
	if external := tx.Bucket([]byte("_external")); external != nil {
		record, err := dsgGraphBucket(external, id)
		if err != nil {
			return "", "", err
		}
		if err := r.each(record, func(_, _ []byte) error {
			out.ExternalEntries++
			return nil
		}); err != nil {
			return "", "", err
		}
	}
	o.Cache = out
	o.Transitive.CacheRecords = r.cacheClosure(&graph, id)
	return target.snapshot, target.blob, nil
}

func (r *dsgGraphReader) solver(o *DSGDedicatedGraph, tx *bolt.Tx, resultID string) error {
	if tx == nil {
		return ErrDSGExactPreserve
	}
	results, byResult := tx.Bucket([]byte("_result")), tx.Bucket([]byte("_byresult"))
	links, backlinks := tx.Bucket([]byte("_links")), tx.Bucket([]byte("_backlinks"))
	if results == nil || byResult == nil || links == nil || backlinks == nil {
		return ErrDSGExactPreserve
	}
	out := &DSGGraphSolver{}
	targetNodes := make(map[string]struct{})
	err := r.each(results, func(k, v []byte) error {
		node := string(k)
		if v != nil || !dsgGraphSolverKey(node) {
			return ErrDSGExactPreserve
		}
		b := results.Bucket(k)
		raw := b.Get([]byte(resultID))
		if b.Bucket([]byte(resultID)) != nil {
			return ErrDSGExactPreserve
		}
		if raw != nil {
			var result solver.CacheResult
			if len(raw) > 512 || json.Unmarshal(raw, &result) != nil || result.ID != resultID ||
				links.Bucket(k) == nil {
				return ErrDSGExactPreserve
			}
			targetNodes[node] = struct{}{}
			r.edge("solver-result", node, resultID)
		}
		return nil
	})
	if err != nil {
		return err
	}
	reverse, err := dsgGraphBucket(byResult, resultID)
	if err != nil {
		return err
	}
	err = r.each(reverse, func(k, v []byte) error {
		_, ok := targetNodes[string(k)]
		if v == nil || len(v) != 0 || !ok {
			return ErrDSGExactPreserve
		}
		out.ResultNodes++
		return nil
	})
	if err != nil || len(targetNodes) != out.ResultNodes {
		return ErrDSGExactPreserve
	}
	if out.ResultNodes != 0 {
		if err := r.solverLinks(out, links, backlinks, targetNodes); err != nil {
			return err
		}
	}
	o.Solver = out
	return nil
}

type dsgSolverPeer struct {
	parent, child string
}

func (r *dsgGraphReader) solverLinks(out *DSGGraphSolver, links, backlinks *bolt.Bucket, targetNodes map[string]struct{}) error {
	forward := make(map[dsgSolverPeer]struct{})
	err := r.each(links, func(k, v []byte) error {
		source := string(k)
		if v != nil || !dsgGraphSolverKey(source) {
			return ErrDSGExactPreserve
		}
		_, from := targetNodes[source]
		return r.each(links.Bucket(k), func(k, v []byte) error {
			raw, endpoint, ok := bytes.Cut(k, []byte{'@'})
			if v == nil || len(v) != 0 || !ok || !dsgGraphSolverKey(string(endpoint)) {
				return ErrDSGExactPreserve
			}
			dest := string(endpoint)
			_, to := targetNodes[dest]
			if !from && !to {
				return nil
			}
			var link solver.CacheInfoLink
			rev := backlinks.Bucket(endpoint)
			if len(raw) > 512 || json.Unmarshal(raw, &link) != nil || !dsgGraphSolverKey(string(link.Digest)) ||
				(link.Selector != "" && !dsgGraphSolverKey(string(link.Selector))) ||
				rev == nil || rev.Get([]byte(source)) == nil || len(rev.Get([]byte(source))) != 0 {
				return ErrDSGExactPreserve
			}
			forward[dsgSolverPeer{source, dest}] = struct{}{}
			if from {
				out.ChildLinks++
			}
			if to {
				out.ParentLinks++
			}
			canonical, _ := json.Marshal(link)
			r.edge("solver-link", source, dest, string(canonical))
			return nil
		})
	})
	if err != nil {
		return err
	}
	return r.each(backlinks, func(k, v []byte) error {
		dest := string(k)
		if v != nil || !dsgGraphSolverKey(dest) {
			return ErrDSGExactPreserve
		}
		_, to := targetNodes[dest]
		return r.each(backlinks.Bucket(k), func(k, v []byte) error {
			source := string(k)
			_, from := targetNodes[source]
			_, exists := forward[dsgSolverPeer{source, dest}]
			if v == nil || len(v) != 0 || ((from || to) && !exists) {
				return ErrDSGExactPreserve
			}
			return nil
		})
	})
}
