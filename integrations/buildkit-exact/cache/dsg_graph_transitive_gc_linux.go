//go:build linux

package cache

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"strings"

	bolt "go.etcd.io/bbolt"
)

// Counts refer to the union of two directed walks from all metadata aliases
// of the target allocation. Incoming and outgoing exclude the seed nodes.
// Expired objects and flat leases are conservatively retained on both walks.
type DSGTransitiveGC struct {
	ContentNodes         int  `json:"content_nodes"`
	NativeSnapshotNodes  int  `json:"native_snapshot_nodes"`
	Incoming             int  `json:"incoming"`
	Outgoing             int  `json:"outgoing"`
	Roots                int  `json:"roots"`
	ForwardLabels        int  `json:"forward_labels"`
	BackLabels           int  `json:"back_labels"`
	HistoryRoots         int  `json:"history_roots"`
	HistoryChecked       bool `json:"history_checked"`
	FlatOrExpiryPolicies int  `json:"flat_or_expiry_policies"`
	UnknownGCLabels      int  `json:"unknown_gc_labels"`
	Unresolved           int  `json:"unresolved_resources"`
	UnsupportedResources int  `json:"unsupported_resources"`
}

type dsgGCNode struct{ ns, kind, key string }

type dsgGCWalk struct {
	r             *dsgGraphReader
	out           DSGTransitiveGC
	paths         dsgPaths[dsgGCNode]
	nodes         map[dsgGCNode]bool
	roots         map[dsgGCNode]bool
	seeds         []dsgGCNode
	blob, backing string
}

// Opaque names are stored and hashed only as fixed-width digests. They never
// become public fields, error strings, paths or authority-bearing inputs.
func dsgOpaqueKey(key []byte) string {
	sum := sha256.Sum256(key)
	return hex.EncodeToString(sum[:])
}

func dsgGCKey(ns, kind string, raw []byte) (dsgGCNode, error) {
	if len(raw) == 0 || len(raw) > 1024 {
		return dsgGCNode{}, ErrDSGExactPreserve
	}
	switch kind {
	case "content":
		if !dsgGraphSHA256(string(raw)) {
			return dsgGCNode{}, ErrDSGExactPreserve
		}
	case "snapshot":
		if !dsgGraphSnapshotKey(string(raw)) {
			return dsgGCNode{}, ErrDSGExactPreserve
		}
	}
	return dsgGCNode{ns, kind, dsgOpaqueKey(raw)}, nil
}

func (w *dsgGCWalk) connect(owner dsgGCNode, kind string, key []byte, relation string, back bool) error {
	dest, err := dsgGCKey(owner.ns, kind, key)
	if err != nil {
		return err
	}
	from, to := owner, dest
	if back {
		from, to = to, from
	}
	w.paths.add(from, to)
	w.r.edge("containerd-transitive", relation, owner.ns, from.kind, from.key, to.kind, to.key)
	return nil
}

func (r *dsgGraphReader) gcClosure(tx, history *bolt.Tx, snapshot, blob string) (*DSGTransitiveGC, error) {
	root := tx.Bucket([]byte("v1"))
	w := &dsgGCWalk{r: r, paths: dsgNewPaths[dsgGCNode](), nodes: make(map[dsgGCNode]bool),
		roots: make(map[dsgGCNode]bool), blob: blob}
	if native := dsgGraphNativeBucket(root.Bucket([]byte(dsgGraphNamespace))); native != nil {
		if record := native.Bucket([]byte(snapshot)); record != nil {
			key := record.Get([]byte("name"))
			if len(key) > 512 || !dsgNativeKey(string(key)) {
				return nil, ErrDSGExactPreserve
			}
			w.backing = string(key)
		}
	}
	err := r.each(root, func(k, v []byte) error {
		if v != nil {
			if string(k) == "version" {
				return nil
			}
			return ErrDSGExactPreserve
		}
		ns := dsgOpaqueKey(k)
		return w.namespace(root.Bucket(k), ns)
	})
	if err != nil {
		return nil, err
	}
	if history != nil {
		ns := dsgOpaqueKey([]byte("buildkit_history"))
		_, err := r.eachHistoryDescriptor(history, func(ordinal, role, blob string) error {
			node, err := dsgGCKey(ns, "history", []byte(ordinal))
			if err != nil {
				return err
			}
			w.nodes[node], w.roots[node] = true, true
			return w.connect(node, "content", []byte(blob), "history-"+role, false)
		})
		w.out.HistoryChecked = err == nil
	}
	// A reference to a missing target blob is still evidence. Include each
	// namespace that contains or names that digest, even without its row.
	if blob != "" {
		key := dsgOpaqueKey([]byte(blob))
		candidates := map[dsgGCNode]bool{{dsgOpaqueKey([]byte(dsgGraphNamespace)), "content", key}: true}
		for node := range w.nodes {
			if node.kind == "content" && node.key == key {
				candidates[node] = true
			}
		}
		for node := range w.paths.forward {
			if node.kind == "content" && node.key == key {
				candidates[node] = true
			}
		}
		for node := range w.paths.reverse {
			if node.kind == "content" && node.key == key {
				candidates[node] = true
			}
		}
		// Walk order only affects the private queue; counts are commutative.
		for node := range candidates {
			w.seeds = append(w.seeds, node)
		}
	}
	prev, next := dsgReach(w.seeds, w.paths.reverse), dsgReach(w.seeds, w.paths.forward)
	w.out.Incoming, w.out.Outgoing = len(prev)-len(w.seeds), len(next)-len(w.seeds)
	for node := range prev {
		if w.roots[node] {
			w.out.Roots++
			if node.kind == "history" {
				w.out.HistoryRoots++
			}
		}
	}
	for node := range next {
		prev[node] = true
	}
	for node := range prev {
		switch node.kind {
		case "snapshot":
			w.out.NativeSnapshotNodes++
		case "content":
			w.out.ContentNodes++
		}
		if !w.nodes[node] {
			w.out.Unresolved++
		}
	}
	r.gcWalk = w
	return &w.out, nil
}

func (w *dsgGCWalk) namespace(ns *bolt.Bucket, nsID string) error {
	return w.r.each(ns, func(k, v []byte) error {
		if v != nil {
			return ErrDSGExactPreserve
		}
		b := ns.Bucket(k)
		switch string(k) {
		case "labels":
			// Namespace labels are not visited by the pinned GC.
			return nil
		case "content":
			return w.r.each(b, func(k, v []byte) error {
				if v != nil {
					return ErrDSGExactPreserve
				}
				switch string(k) {
				case "blob":
					return w.objects(b.Bucket(k), nsID, "content")
				case "ingests":
					return w.objects(b.Bucket(k), nsID, "ingest")
				default:
					w.out.UnsupportedResources++
				}
				return nil
			})
		case "snapshots":
			return w.r.each(b, func(k, v []byte) error {
				if v != nil {
					return ErrDSGExactPreserve
				}
				if string(k) != "native" {
					w.out.UnsupportedResources++
					return nil
				}
				return w.objects(b.Bucket(k), nsID, "snapshot")
			})
		case "images":
			return w.objects(b, nsID, "image")
		case "leases":
			return w.objects(b, nsID, "lease")
		case "containers":
			return w.objects(b, nsID, "container")
		case "sandboxes":
			return w.objects(b, nsID, "sandbox")
		default:
			w.out.UnsupportedResources++
		}
		return nil
	})
}

func (w *dsgGCWalk) objects(objects *bolt.Bucket, ns, kind string) error {
	return w.r.each(objects, func(k, v []byte) error {
		if v != nil {
			return ErrDSGExactPreserve
		}
		node, err := dsgGCKey(ns, kind, k)
		if err != nil {
			return err
		}
		w.nodes[node] = true
		object := objects.Bucket(k)
		switch kind {
		case "content":
			if w.blob != "" && string(k) == w.blob {
				w.r.edge("containerd-gc-seed", ns, kind, node.key)
			}
		case "snapshot":
			name := object.Get([]byte("name"))
			if len(name) == 0 || len(name) > 512 {
				return ErrDSGExactPreserve
			}
			if w.backing != "" && string(name) == w.backing {
				w.seeds = append(w.seeds, node)
				w.r.edge("containerd-gc-seed", ns, kind, node.key)
			}
			if object.Bucket([]byte("parent")) != nil {
				return ErrDSGExactPreserve
			}
			if p := object.Get([]byte("parent")); len(p) != 0 {
				if err := w.connect(node, "snapshot", p, "snapshot-parent", false); err != nil {
					return err
				}
			}
		case "image", "container", "ingest":
			w.roots[node] = true
			s, err := dsgReadExternalStructural(kind, object)
			if err != nil {
				return err
			}
			if s.snapshot != "" {
				if s.snapshotter == "native" {
					if err := w.connect(node, "snapshot", []byte(s.snapshot), "container-snapshot", false); err != nil {
						return err
					}
				} else {
					w.out.UnsupportedResources++
				}
			}
			if s.blob != "" {
				if err := w.connect(node, "content", []byte(s.blob), kind+"-content", false); err != nil {
					return err
				}
			}
			if s.snapshotter != "" {
				if s.snapshotter == "native" {
					if err := w.connect(node, "snapshot", []byte(s.snapshot), "container-snapshot", false); err != nil {
						return err
					}
				} else {
					w.out.UnsupportedResources++
				}
			}
		case "lease":
			w.roots[node] = true
			if err := w.lease(node, object); err != nil {
				return err
			}
		case "sandbox":
			w.roots[node] = true
		}
		labels, err := dsgGraphBucket(object, "labels")
		if err != nil {
			return err
		}
		return w.r.each(labels, func(k, v []byte) error {
			if v == nil {
				return ErrDSGExactPreserve
			}
			if !bytes.HasPrefix(k, []byte("containerd.io/gc.")) {
				return nil
			}
			return w.label(node, string(k[len("containerd.io/gc."):]), v)
		})
	})
}

func (w *dsgGCWalk) lease(node dsgGCNode, object *bolt.Bucket) error {
	resources := func(b *bolt.Bucket, kind string) error {
		return w.r.each(b, func(k, v []byte) error {
			if v == nil || len(v) != 0 {
				return ErrDSGExactPreserve
			}
			return w.connect(node, kind, k, "lease-resource", false)
		})
	}
	return w.r.each(object, func(k, v []byte) error {
		if v != nil {
			if string(k) != "createdat" {
				w.out.UnsupportedResources++
			}
			return nil
		}
		b := object.Bucket(k)
		switch string(k) {
		case "labels":
			return nil
		case "content":
			return resources(b, "content")
		case "images":
			return resources(b, "image")
		case "ingests":
			return resources(b, "ingest")
		case "snapshots":
			return w.r.each(b, func(k, v []byte) error {
				if v != nil {
					return ErrDSGExactPreserve
				}
				if string(k) != "native" {
					w.out.UnsupportedResources++
					return nil
				}
				return resources(b.Bucket(k), "snapshot")
			})
		default:
			w.out.UnsupportedResources++
		}
		return nil
	})
}

// Containerd 2.4.2 core/metadata/gc.go accepts "." or "/" names on
// content/image/container references and "/" names after the snapshotter.
// Conditions and plugin-defined resource types require preservation.
func (w *dsgGCWalk) label(owner dsgGCNode, label string, value []byte) error {
	switch {
	case label == "root" && (owner.kind == "content" || owner.kind == "snapshot"):
		w.roots[owner] = true
		w.r.edge("containerd-gc-root", owner.ns, owner.kind, owner.key)
		return nil
	case (label == "flat" && owner.kind == "lease") ||
		(label == "expire" && (owner.kind == "image" || owner.kind == "lease")):
		w.out.FlatOrExpiryPolicies++
		w.r.edge("containerd-gc-policy", owner.ns, owner.kind, owner.key, label)
		return nil
	}
	dir, target, ok := strings.Cut(label, ".")
	back := dir == "bref"
	if !ok || (!back && dir != "ref") || owner.kind == "ingest" || owner.kind == "lease" ||
		(back && owner.kind == "sandbox") {
		w.out.UnknownGCLabels++
		return nil
	}
	kind := ""
	if snapshotter, ok := strings.CutPrefix(target, "snapshot."); ok {
		snapshotter, _, _ = strings.Cut(snapshotter, "/")
		if snapshotter == "native" {
			kind = "snapshot"
		}
	} else {
		for _, typ := range []string{"content", "image", "container"} {
			if target == typ || strings.HasPrefix(target, typ+".") || strings.HasPrefix(target, typ+"/") {
				if typ != "container" || back {
					kind = typ
				}
				break
			}
		}
	}
	if kind == "" {
		w.out.UnknownGCLabels++
		return nil
	}
	if err := w.connect(owner, kind, value, dir, back); err != nil {
		return err
	}
	if back {
		w.out.BackLabels++
	} else {
		w.out.ForwardLabels++
	}
	return nil
}
