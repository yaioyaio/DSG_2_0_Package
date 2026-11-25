//go:build linux

package cache

import (
	"bytes"
	"encoding/json"

	"github.com/moby/buildkit/solver"
	bolt "go.etcd.io/bbolt"
)

// These are counts of potential retention paths. They do not authorize GC.
type DSGGraphTransitive struct {
	Solver   *DSGTransitiveSolver `json:"solver,omitempty"`
	GCLabels *DSGTransitiveGC     `json:"gc_labels,omitempty"`
}

type DSGTransitiveSolver struct {
	ResultNodes int `json:"result_nodes"`
	Ancestors   int `json:"ancestors"`
	Descendants int `json:"descendants"`
	Links       int `json:"links"` // Validated forward entries on the two walks.
}

type dsgPaths[T comparable] struct {
	forward, reverse map[T][]T
}

func dsgNewPaths[T comparable]() dsgPaths[T] {
	return dsgPaths[T]{forward: make(map[T][]T), reverse: make(map[T][]T)}
}

func (p dsgPaths[T]) add(from, to T) {
	p.forward[from] = append(p.forward[from], to)
	p.reverse[to] = append(p.reverse[to], from)
}

// Every adjacency entry is produced by a bounded store scan. Mark on enqueue
// so cycles and duplicate roots cannot multiply the amount of work.
func dsgReach[T comparable](seeds []T, adjacency map[T][]T) map[T]bool {
	seen := make(map[T]bool)
	queue := make([]T, 0, len(seeds))
	push := func(id T) {
		if !seen[id] {
			seen[id] = true
			queue = append(queue, id)
		}
	}
	for _, id := range seeds {
		push(id)
	}
	for i := 0; i < len(queue); i++ {
		for _, next := range adjacency[queue[i]] {
			push(next)
		}
	}
	return seen
}

func (r *dsgGraphReader) extendedGraph(o *DSGDedicatedGraph, solverTx, cacheTx, containerdTx, history *bolt.Tx, worker, id, snapshot, blob string, warn func(string)) {
	o.Transitive = &DSGGraphTransitive{}
	if o.Solver != nil {
		s, err := r.solverClosure(solverTx, worker+"::"+id, o.Solver.ResultNodes)
		if err == nil {
			o.Transitive.Solver = s
		} else {
			warn("solver_transitive_link_indexes")
		}
	}
	if o.Containerd != nil {
		g, err := r.gcClosure(containerdTx, history, snapshot, blob)
		if err == nil {
			o.Transitive.GCLabels = g
			if !g.HistoryChecked {
				warn("history_transitive_descriptor_roots")
			}
			if g.UnknownGCLabels != 0 {
				warn("unsupported_containerd_gc_labels")
			}
			if g.Unresolved != 0 {
				warn("containerd_graph_missing_referent")
			}
			if g.UnsupportedResources != 0 {
				warn("unsupported_containerd_resource_or_collector")
			}
		} else {
			warn("containerd_transitive_gc_projection")
		}
	}
	external, reasons := r.externalRefs(cacheTx, containerdTx, history, id, snapshot, blob)
	o.External = external
	for _, code := range reasons {
		warn(code)
	}
}

func (r *dsgGraphReader) solverClosure(tx *bolt.Tx, resultID string, expected int) (*DSGTransitiveSolver, error) {
	// Called only after _result and _byresult agree on all target occurrences.
	byResult := tx.Bucket([]byte("_byresult"))
	links, backs := tx.Bucket([]byte("_links")), tx.Bucket([]byte("_backlinks"))
	var seeds []string
	if err := r.each(byResult.Bucket([]byte(resultID)), func(k, _ []byte) error {
		seeds = append(seeds, string(k))
		return nil
	}); err != nil || len(seeds) != expected {
		return nil, ErrDSGExactPreserve
	}
	paths := dsgNewPaths[string]()
	peers := make(map[dsgSolverPeer]int)
	err := r.each(links, func(k, v []byte) error {
		from := string(k)
		if v != nil || !dsgGraphSolverKey(from) {
			return ErrDSGExactPreserve
		}
		return r.each(links.Bucket(k), func(k, v []byte) error {
			raw, end, ok := bytes.Cut(k, []byte("@"))
			to := string(end)
			if !ok || v == nil || len(v) != 0 || !dsgGraphSolverKey(to) || len(raw) > 512 {
				return ErrDSGExactPreserve
			}
			var link solver.CacheInfoLink
			if json.Unmarshal(raw, &link) != nil || !dsgGraphSolverKey(string(link.Digest)) ||
				(link.Selector != "" && !dsgGraphSolverKey(string(link.Selector))) {
				return ErrDSGExactPreserve
			}
			// Pinned AddLink writes exactly json.Marshal(link). Reject unknown
			// fields, duplicates or non-canonical representations as ambiguity.
			canonical, err := json.Marshal(link)
			if err != nil || !bytes.Equal(canonical, raw) {
				return ErrDSGExactPreserve
			}
			back := backs.Bucket(end)
			if back == nil || back.Get([]byte(from)) == nil || len(back.Get([]byte(from))) != 0 {
				return ErrDSGExactPreserve
			}
			pair := dsgSolverPeer{from, to}
			if peers[pair] == 0 {
				paths.add(from, to)
			}
			peers[pair]++
			r.edge("solver-transitive-link", from, to, string(canonical))
			return nil
		})
	})
	if err != nil {
		return nil, err
	}
	err = r.each(backs, func(k, v []byte) error {
		to := string(k)
		if v != nil || !dsgGraphSolverKey(to) {
			return ErrDSGExactPreserve
		}
		return r.each(backs.Bucket(k), func(k, v []byte) error {
			if v == nil || len(v) != 0 || peers[dsgSolverPeer{string(k), to}] == 0 {
				return ErrDSGExactPreserve
			}
			return nil
		})
	})
	if err != nil {
		return nil, err
	}
	prev, next := dsgReach(seeds, paths.reverse), dsgReach(seeds, paths.forward)
	out := &DSGTransitiveSolver{ResultNodes: len(seeds), Ancestors: len(prev) - len(seeds), Descendants: len(next) - len(seeds)}
	for pair, count := range peers {
		if next[pair.parent] || prev[pair.child] {
			out.Links += count
		}
	}
	return out, nil
}
