//go:build linux

package cache

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"strings"
	"testing"

	"github.com/moby/buildkit/solver"
	"github.com/opencontainers/go-digest"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

func TestDSGGraphTransitiveSolver(t *testing.T) {
	f := dsgNewGraphFixture(t)
	grandchild := strings.Repeat("g", 25)
	link, err := json.Marshal(solver.CacheInfoLink{Digest: digest.FromString("transitive solver fixture")})
	require.NoError(t, err)
	require.NoError(t, f.solver.Update(func(tx *bolt.Tx) error {
		for _, pair := range [][2]string{{dsgGraphTestChild, grandchild}, {grandchild, dsgGraphTestChild}} {
			b := dsgGraphTestBucket(t, tx, "_links", pair[0])
			require.NoError(t, b.Put([]byte(string(link)+"@"+pair[1]), nil))
			b = dsgGraphTestBucket(t, tx, "_backlinks", pair[1])
			require.NoError(t, b.Put([]byte(pair[0]), nil))
		}
		return nil
	}))
	g := f.observe(t)
	require.Equal(t, &DSGTransitiveSolver{ResultNodes: 1, Ancestors: 1, Descendants: 2, Links: 4}, g.Transitive.Solver)
	require.Equal(t, g, f.observe(t))
	require.NoError(t, f.solver.Update(func(tx *bolt.Tx) error {
		return dsgGraphTestBucket(t, tx, "_backlinks", grandchild).Delete([]byte(dsgGraphTestChild))
	}))
	g = f.observe(t)
	require.NotNil(t, g.Solver)
	require.Nil(t, g.Transitive.Solver)
	require.Contains(t, g.Unverified, "solver_transitive_link_indexes")
	require.Equal(t, "preserve", g.Disposition)
}

func TestDSGGraphTransitiveGCRetention(t *testing.T) {
	f := dsgNewGraphFixture(t)
	target := "sha256:" + strings.Repeat("a", 64)
	second := "sha256:" + strings.Repeat("b", 64)
	third := "sha256:" + strings.Repeat("c", 64)
	before := f.observe(t)
	require.NotNil(t, before.Transitive.GCLabels)
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		label := func(owner, key, value string) {
			b := dsgGraphTestBucket(t, tx, "v1", "buildkit", "content", "blob", owner, "labels")
			require.NoError(t, b.Put([]byte("containerd.io/gc."+key), []byte(value)))
		}
		label(second, "ref.content.chain", target)
		label(second, "bref.content/owner", third)
		label(third, "root", "arbitrary label metadata")
		label(target, "ref.snapshot.native/link", dsgGraphTestParent)
		return nil
	}))
	g := f.observe(t)
	gc := g.Transitive.GCLabels
	require.NotNil(t, gc)
	require.Equal(t, 0, gc.UnknownGCLabels)
	require.Equal(t, 0, gc.Unresolved)
	require.Equal(t, 2, gc.ForwardLabels)
	require.Equal(t, 1, gc.BackLabels)
	require.Equal(t, before.Transitive.GCLabels.Incoming+2, gc.Incoming)
	require.Equal(t, before.Transitive.GCLabels.Roots+1, gc.Roots)
	require.NotEqual(t, before.EvidenceHash, g.EvidenceHash)
	require.Equal(t, g, f.observe(t))
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		b := dsgGraphTestBucket(t, tx, "v1", "other", "content", "blob", target, "labels")
		require.NoError(t, b.Put([]byte("containerd.io/gc.cond.snapshot.native"), []byte("opaque condition")))
		require.NoError(t, b.Put([]byte("containerd.io/gc.ref.content/missing"), []byte("sha256:"+strings.Repeat("d", 64))))
		return nil
	}))
	g = f.observe(t)
	require.Equal(t, 1, g.Transitive.GCLabels.Unresolved)
	require.Equal(t, 1, g.Transitive.GCLabels.UnknownGCLabels)
	require.Contains(t, g.Unverified, "unsupported_containerd_gc_labels")
	require.Contains(t, g.Unverified, "containerd_graph_missing_referent")
	raw, err := json.Marshal(g)
	require.NoError(t, err)
	require.NotContains(t, string(raw), "opaque condition")
	require.NotContains(t, string(raw), "arbitrary label metadata")
}

func TestDSGGraphTransitiveGCRejectsMalformedOrExhaustedScan(t *testing.T) {
	f := dsgNewGraphFixture(t)
	require.NoError(t, f.backend.View(func(tx *bolt.Tx) error {
		r := dsgGraphReader{ctx: context.Background(), remaining: 2, evidence: sha256.New()}
		_, err := r.gcClosure(tx, nil, dsgTarget, "")
		require.ErrorIs(t, err, ErrDSGExactPreserve)
		return nil
	}))
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		b := dsgGraphTestBucket(t, tx, "v1", "buildkit", "snapshots", "native", dsgTarget, "labels")
		return b.Put([]byte("containerd.io/gc.ref.content"), []byte("invalid digest"))
	}))
	g := f.observe(t)
	require.NotNil(t, g.Containerd)
	require.Nil(t, g.Transitive.GCLabels)
	require.Contains(t, g.Unverified, "containerd_transitive_gc_projection")
	require.Equal(t, "preserve", g.Disposition)
}
