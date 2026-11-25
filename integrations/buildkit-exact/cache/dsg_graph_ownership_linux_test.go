//go:build linux

package cache

import (
	"encoding/binary"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

// The record and snapshot intentionally have different identifiers.
const dsgOwnerSnapshot = dsgGraphTestParent

func dsgOwnershipFixture(t *testing.T, hasView bool) (*dsgGraphFixture, *bolt.DB) {
	t.Helper()
	f := &dsgGraphFixture{}
	root := t.TempDir()
	for _, item := range []struct {
		name string
		db   **bolt.DB
	}{
		{"solver.db", &f.solver}, {"cache.db", &f.cache}, {"backend.db", &f.backend}, {"native.db", &f.native},
	} {
		db, err := bolt.Open(filepath.Join(root, item.name), 0600, nil)
		require.NoError(t, err)
		*item.db = db
		t.Cleanup(func() { require.NoError(t, db.Close()) })
	}
	encoded := strings.Repeat("a", 64)
	blob := "sha256:" + encoded
	require.NoError(t, os.MkdirAll(filepath.Join(root, "snapshots", "snapshots", "1"), 0700))
	require.NoError(t, os.MkdirAll(filepath.Join(root, "content", "blobs", "sha256"), 0700))
	require.NoError(t, os.WriteFile(filepath.Join(root, "content", "blobs", "sha256", encoded), []byte("abc"), 0600))
	var err error
	f.worker, err = os.Open(root)
	require.NoError(t, err)
	f.snapshotter, err = os.Open(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, f.worker.Close()); require.NoError(t, f.snapshotter.Close()) })

	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		target := dsgGraphTestBucket(t, tx, "_main", dsgTarget)
		dsgGraphTestMetadata(t, target, keySnapshot, dsgOwnerSnapshot)
		dsgGraphTestMetadata(t, target, keyBlob, blob)
		dsgGraphTestMetadata(t, target, keyCommitted, true)
		return nil
	}))
	require.NoError(t, f.solver.Update(func(tx *bolt.Tx) error {
		for _, b := range []string{"_result", "_byresult", "_links", "_backlinks"} {
			dsgGraphTestBucket(t, tx, b)
		}
		return nil
	}))
	targetBacking := "buildkit/5/" + dsgOwnerSnapshot
	view := dsgOwnerSnapshot + "-view"
	viewBacking := "buildkit/6/" + view
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		require.NoError(t, dsgGraphTestBucket(t, tx, "v1").Put([]byte("version"), binary.AppendVarint(nil, 4)))
		put := func(path []string, k string, v []byte) {
			require.NoError(t, dsgGraphTestBucket(t, tx, append([]string{"v1", "buildkit"}, path...)...).Put([]byte(k), v))
		}
		put([]string{"snapshots", "native", dsgOwnerSnapshot}, "name", []byte(targetBacking))
		put([]string{"content", "blob", blob}, "size", binary.AppendVarint(nil, 3))
		put([]string{"content", "blob", blob, "labels"}, blobVariantGCLabel+blob, []byte(blob))
		put([]string{"leases", dsgTarget, "snapshots", "native"}, dsgOwnerSnapshot, nil)
		put([]string{"leases", dsgTarget, "content"}, blob, nil)
		put([]string{"leases", dsgTarget + "-variants", "content"}, blob, nil)
		if hasView {
			put([]string{"snapshots", "native", view}, "name", []byte(viewBacking))
			put([]string{"snapshots", "native", view}, "parent", []byte(dsgOwnerSnapshot))
			put([]string{"snapshots", "native", dsgOwnerSnapshot, "children"}, view, nil)
			put([]string{"leases", dsgTarget + "-view", "snapshots", "native"}, view, nil)
		}
		return nil
	}))
	require.NoError(t, f.native.Update(func(tx *bolt.Tx) error {
		idx := dsgGraphTestBucket(t, tx, "v1", "parents")
		record := dsgGraphTestBucket(t, tx, "v1", "snapshots", targetBacking)
		require.NoError(t, record.Put([]byte("id"), binary.AppendUvarint(nil, 1)))
		require.NoError(t, record.Put([]byte("kind"), []byte{byte(snapshots.KindCommitted)}))
		if hasView {
			child := dsgGraphTestBucket(t, tx, "v1", "snapshots", viewBacking)
			require.NoError(t, child.Put([]byte("id"), binary.AppendUvarint(nil, 2)))
			require.NoError(t, child.Put([]byte("kind"), []byte{byte(snapshots.KindView)}))
			require.NoError(t, child.Put([]byte("parent"), []byte(targetBacking)))
			require.NoError(t, idx.Put(dsgGraphNativeIndexKey(1, 2), []byte(viewBacking)))
		}
		return nil
	}))
	return f, dsgNewExternalHistory(t, "2")
}

func dsgObserveOwnershipFixture(t *testing.T, f *dsgGraphFixture, history *bolt.DB) *DSGDedicatedGraph {
	t.Helper()
	var txs []*bolt.Tx
	for _, db := range []*bolt.DB{f.solver, f.cache, f.backend, f.native, history} {
		tx, err := db.Begin(false)
		require.NoError(t, err)
		defer tx.Rollback()
		txs = append(txs, tx)
	}
	g, err := dsgObserveNativeGraph(t.Context(), dsgGraphTestWorker, dsgTarget, txs[0], txs[1], txs[2], txs[3], int(f.worker.Fd()), int(f.snapshotter.Fd()), txs[4])
	require.NoError(t, err)
	return g
}

func TestDSGRetentionOwnership(t *testing.T) {
	for _, tc := range []struct {
		name            string
		view            bool
		roots, children int
	}{{"record and variants", false, 2, 0}, {"read-only view", true, 3, 1}} {
		t.Run(tc.name, func(t *testing.T) {
			f, h := dsgOwnershipFixture(t, tc.view)
			o := dsgObserveOwnershipFixture(t, f, h)
			require.Equal(t, "exact_record_resources", o.Ownership.Reason, "%v", o.Unverified)
			require.True(t, o.Ownership.Checked && o.Ownership.Exclusive && o.Ownership.RecordLease && o.Ownership.VariantLease)
			require.Equal(t, tc.view, o.Ownership.ViewLease)
			require.Equal(t, tc.roots, o.Ownership.OwnedRoots)
			require.Equal(t, tc.children, o.Ownership.OwnedChildSnapshots)
			require.Equal(t, "preserve", o.Disposition)
		})
	}
}

func TestDSGRetentionOwnershipReleasedView(t *testing.T) {
	f, h := dsgOwnershipFixture(t, true)
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		return dsgGraphTestBucket(t, tx, "v1", "buildkit", "leases").DeleteBucket([]byte(dsgTarget + "-view"))
	}))
	g := dsgObserveOwnershipFixture(t, f, h)
	require.Equal(t, "exact_record_resources", g.Ownership.Reason)
	require.True(t, g.Ownership.Exclusive)
	require.False(t, g.Ownership.ViewLease)
	require.Equal(t, 2, g.Ownership.OwnedRoots)
	require.Equal(t, 1, g.Ownership.OwnedChildSnapshots)
}

func TestDSGRetentionOwnershipRefusesAmbiguity(t *testing.T) {
	view := dsgOwnerSnapshot + "-view"
	blob := "sha256:" + strings.Repeat("a", 64)
	for _, tc := range []struct {
		name   string
		change func(*testing.T, *dsgGraphFixture)
	}{
		{"foreign child lease", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				return dsgGraphTestBucket(t, tx, "v1", "buildkit", "leases", "unrelated", "snapshots", "native").Put([]byte(view), nil)
			}))
		}},
		{"cache maps child", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
				dsgGraphTestMetadata(t, dsgGraphTestBucket(t, tx, "_main", dsgOther), keySnapshot, view)
				return nil
			}))
		}},
		{"child backing alias", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				return dsgGraphTestBucket(t, tx, "v1", "other", "snapshots", "native", dsgOther).Put([]byte("name"), []byte("buildkit/6/"+view))
			}))
		}},
		{"stale reverse index", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				return dsgGraphTestBucket(t, tx, "v1", "buildkit", "snapshots", "native", dsgOwnerSnapshot, "children").Delete([]byte(view))
			}))
		}},
		{"foreign content reference", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				return dsgGraphTestBucket(t, tx, "v1", "buildkit", "content", "blob", "sha256:"+strings.Repeat("b", 64), "labels").Put([]byte("containerd.io/gc.ref.content"), []byte(blob))
			}))
		}},
		{"root label on child", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				return dsgGraphTestBucket(t, tx, "v1", "buildkit", "snapshots", "native", view, "labels").Put([]byte("containerd.io/gc.root"), nil)
			}))
		}},
		{"unexpected private child directory", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, os.Mkdir(filepath.Join(f.snapshotter.Name(), "snapshots", "2"), 0700))
		}},
		{"opaque lease resource", func(t *testing.T, f *dsgGraphFixture) {
			require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
				dsgGraphTestBucket(t, tx, "v1", "buildkit", "leases", dsgTarget, "future")
				return nil
			}))
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f, h := dsgOwnershipFixture(t, true)
			tc.change(t, f)
			g := dsgObserveOwnershipFixture(t, f, h)
			require.False(t, g.Ownership.Exclusive)
			if tc.name == "foreign content reference" {
				require.Equal(t, dsgOpaqueKey([]byte("sha256:"+strings.Repeat("b", 64))), g.Ownership.IncomingContentKeySHA256)
				require.Equal(t, 3, g.Ownership.OwnedRoots)
				require.Equal(t, 1, g.Ownership.OwnedChildSnapshots)
				require.Equal(t, "foreign_or_unresolved_retention_edge", g.Ownership.Reason)
			}
			require.Equal(t, "preserve", g.Disposition)
		})
	}
}
