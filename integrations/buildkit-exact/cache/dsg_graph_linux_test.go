//go:build linux

package cache

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/moby/buildkit/cache/metadata"
	"github.com/moby/buildkit/solver"
	"github.com/opencontainers/go-digest"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

const (
	dsgGraphTestParent = "000000000000000000000000c"
	dsgGraphTestWorker = "000000000000000000000000d"
	dsgGraphTestNode   = "000000000000000000000000e"
	dsgGraphTestChild  = "000000000000000000000000f"
)

type dsgGraphFixture struct {
	solver, cache, backend, native *bolt.DB
	worker, snapshotter            *os.File
	blobFile, allocation           string
}

func dsgGraphTestBucket(t *testing.T, tx *bolt.Tx, path ...string) *bolt.Bucket {
	t.Helper()
	b, err := tx.CreateBucketIfNotExists([]byte(path[0]))
	require.NoError(t, err)
	for _, key := range path[1:] {
		b, err = b.CreateBucketIfNotExists([]byte(key))
		require.NoError(t, err)
	}
	return b
}

func dsgGraphTestPut(t *testing.T, b *bolt.Bucket, key string, value any) {
	t.Helper()
	raw, err := json.Marshal(value)
	require.NoError(t, err)
	require.NoError(t, b.Put([]byte(key), raw))
}

func dsgGraphTestMetadata(t *testing.T, b *bolt.Bucket, key string, value any) {
	t.Helper()
	v, err := metadata.NewValue(value)
	require.NoError(t, err)
	dsgGraphTestPut(t, b, key, v)
}

func dsgNewGraphFixture(t *testing.T) *dsgGraphFixture {
	t.Helper()
	root := t.TempDir()
	f := &dsgGraphFixture{}
	for _, item := range []struct {
		name string
		dst  **bolt.DB
	}{
		{"solver.db", &f.solver}, {"cache.db", &f.cache},
		{"containerd.db", &f.backend}, {"native.db", &f.native},
	} {
		db, err := bolt.Open(filepath.Join(root, item.name), 0600, nil)
		require.NoError(t, err)
		*item.dst = db
		t.Cleanup(func() { require.NoError(t, db.Close()) })
	}

	encoded := strings.Repeat("a", 64)
	blob := "sha256:" + encoded
	f.blobFile = filepath.Join(root, "content", "blobs", "sha256", encoded)
	require.NoError(t, os.MkdirAll(filepath.Dir(f.blobFile), 0700))
	require.NoError(t, os.WriteFile(f.blobFile, []byte("abc"), 0600))
	f.allocation = filepath.Join(root, "snapshots", "snapshots", "2")
	require.NoError(t, os.MkdirAll(f.allocation, 0700))
	var err error
	f.worker, err = os.Open(root)
	require.NoError(t, err)
	f.snapshotter, err = os.Open(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	t.Cleanup(func() {
		require.NoError(t, f.worker.Close())
		require.NoError(t, f.snapshotter.Close())
	})

	require.NoError(t, f.cache.Update(func(tx *bolt.Tx) error {
		target := dsgGraphTestBucket(t, tx, "_main", dsgTarget)
		dsgGraphTestMetadata(t, target, keyParent, dsgGraphTestParent)
		dsgGraphTestMetadata(t, target, keyBlob, blob)
		dsgGraphTestMetadata(t, target, keyCommitted, true)
		require.NoError(t, target.Put([]byte(keyEqualMutable), nil))
		dsgGraphTestBucket(t, tx, "_main", dsgGraphTestParent)
		other := dsgGraphTestBucket(t, tx, "_main", dsgOther)
		dsgGraphTestMetadata(t, other, keySnapshot, dsgTarget)
		dsgGraphTestMetadata(t, other, keyBlob, blob)
		dsgGraphTestMetadata(t, other, keyLowerDiffParent, dsgTarget)
		dsgGraphTestMetadata(t, other, keyEqualMutable, dsgTarget)
		return nil
	}))
	require.NoError(t, f.solver.Update(func(tx *bolt.Tx) error {
		result := dsgGraphTestWorker + "::" + dsgTarget
		dsgGraphTestPut(t, dsgGraphTestBucket(t, tx, "_result", dsgGraphTestNode), result,
			solver.CacheResult{ID: result, CreatedAt: time.Unix(1, 0).UTC()})
		require.NoError(t, dsgGraphTestBucket(t, tx, "_byresult", result).Put([]byte(dsgGraphTestNode), nil))
		for _, relation := range [][2]string{
			{dsgGraphTestParent, dsgGraphTestNode}, {dsgGraphTestNode, dsgGraphTestChild},
		} {
			dt, err := json.Marshal(solver.CacheInfoLink{Digest: digest.FromString("dsg graph test relation")})
			require.NoError(t, err)
			require.NoError(t, dsgGraphTestBucket(t, tx, "_links", relation[0]).Put([]byte(string(dt)+"@"+relation[1]), nil))
			require.NoError(t, dsgGraphTestBucket(t, tx, "_backlinks", relation[1]).Put([]byte(relation[0]), nil))
		}
		return nil
	}))
	require.NoError(t, f.backend.Update(func(tx *bolt.Tx) error {
		require.NoError(t, dsgGraphTestBucket(t, tx, "v1").Put([]byte("version"), binary.AppendVarint(nil, 4)))
		for _, ns := range []string{"buildkit", "other"} {
			b := dsgGraphTestBucket(t, tx, "v1", ns, "content", "blob", blob)
			require.NoError(t, b.Put([]byte("size"), binary.AppendVarint(nil, 3)))
			b = dsgGraphTestBucket(t, tx, "v1", ns, "leases", dsgTarget, "content")
			require.NoError(t, b.Put([]byte(blob), nil))
		}
		names := []struct{ record, native, parent string }{
			{dsgGraphTestParent, "buildkit/1/" + dsgGraphTestParent, ""},
			{dsgTarget, "buildkit/2/" + dsgTarget, dsgGraphTestParent},
			{dsgGraphTestChild, "buildkit/3/" + dsgGraphTestChild, dsgTarget},
		}
		for _, n := range names {
			b := dsgGraphTestBucket(t, tx, "v1", "buildkit", "snapshots", "native", n.record)
			require.NoError(t, b.Put([]byte("name"), []byte(n.native)))
			if n.parent != "" {
				require.NoError(t, b.Put([]byte("parent"), []byte(n.parent)))
			}
		}
		b := dsgGraphTestBucket(t, tx, "v1", "other", "snapshots", "native", dsgOther)
		require.NoError(t, b.Put([]byte("name"), []byte(names[1].native)))
		b = dsgGraphTestBucket(t, tx, "v1", "buildkit", "leases", dsgTarget, "snapshots", "native")
		require.NoError(t, b.Put([]byte(dsgTarget), nil))
		b = dsgGraphTestBucket(t, tx, "v1", "buildkit", "leases", dsgOther, "snapshots", "native")
		require.NoError(t, b.Put([]byte(dsgTarget), nil))
		return nil
	}))
	require.NoError(t, f.native.Update(func(tx *bolt.Tx) error {
		names := []string{"buildkit/1/" + dsgGraphTestParent, "buildkit/2/" + dsgTarget, "buildkit/3/" + dsgGraphTestChild}
		for i, name := range names {
			id := uint64(i + 1)
			b := dsgGraphTestBucket(t, tx, "v1", "snapshots", name)
			require.NoError(t, b.Put([]byte("id"), binary.AppendUvarint(nil, id)))
			require.NoError(t, b.Put([]byte("kind"), []byte{byte(snapshots.KindCommitted)}))
			if i > 0 {
				require.NoError(t, b.Put([]byte("parent"), []byte(names[i-1])))
				index := dsgGraphTestBucket(t, tx, "v1", "parents")
				require.NoError(t, index.Put(dsgGraphNativeIndexKey(id-1, id), []byte(name)))
			}
		}
		return nil
	}))
	return f
}

func (f *dsgGraphFixture) observe(t *testing.T) *DSGDedicatedGraph {
	t.Helper()
	open := func(db *bolt.DB) *bolt.Tx {
		tx, err := db.Begin(false)
		require.NoError(t, err)
		return tx
	}
	s, c, b, n := open(f.solver), open(f.cache), open(f.backend), open(f.native)
	defer s.Rollback()
	defer c.Rollback()
	defer b.Rollback()
	defer n.Rollback()
	g, err := dsgObserveNativeGraph(context.Background(), dsgGraphTestWorker, dsgTarget,
		s, c, b, n, int(f.worker.Fd()), int(f.snapshotter.Fd()))
	require.NoError(t, err)
	return g
}

func TestDSGGraphProjectsNativeAdjacency(t *testing.T) {
	f := dsgNewGraphFixture(t)
	g := f.observe(t)
	require.Equal(t, "preserve", g.Disposition)
	require.Len(t, g.EvidenceHash, 64)
	require.Contains(t, g.Unverified, "native_allocation_contents")
	require.NotContains(t, g.Unverified, "cache_record_transitive_parents_and_aliases")
	require.Equal(t, &DSGTransitiveCache{Records: 3, Ancestors: 1, AliasRecords: 1, ParentLinks: 2, AliasLinks: 1}, g.Transitive.CacheRecords)
	require.Equal(t, &DSGGraphCache{
		Committed: true, SnapshotFallback: true, Parents: 1, Children: 1, AliasesIn: 1, SnapshotPeers: 1, BlobPeers: 1,
	}, g.Cache)
	require.Equal(t, &DSGGraphSolver{ResultNodes: 1, ParentLinks: 1, ChildLinks: 1}, g.Solver)
	require.Equal(t, &DSGGraphBackend{
		Namespaces: 2, RecordLease: true, RecordLeaseSnapshots: 1, RecordLeaseBlobs: 1,
		TargetLeaseSnapshot: true, TargetLeaseBlob: true, SnapshotLeases: 2, BlobLeases: 2,
		SnapshotMapping: true, SnapshotParents: 1, SnapshotChildren: 1, NativeSnapshotAliases: 1, BlobNamespaces: 2,
	}, g.Containerd)
	require.Equal(t, &DSGGraphSnapshot{Kind: snapshots.KindCommitted.String(), Parents: 1, Children: 1, DirectoryPresent: true}, g.Snapshot)
	require.Equal(t, &DSGGraphBlob{Size: 3}, g.Blob)
	require.Equal(t, g, f.observe(t))
}

func TestDSGGraphRejectsDisagreeingIndexes(t *testing.T) {
	f := dsgNewGraphFixture(t)
	before := f.observe(t)
	require.NoError(t, f.solver.Update(func(tx *bolt.Tx) error {
		return tx.Bucket([]byte("_backlinks")).Bucket([]byte(dsgGraphTestNode)).Delete([]byte(dsgGraphTestParent))
	}))
	require.NoError(t, f.native.Update(func(tx *bolt.Tx) error {
		return tx.Bucket([]byte("v1")).Bucket([]byte("parents")).Delete(dsgGraphNativeIndexKey(2, 3))
	}))
	g := f.observe(t)
	require.Equal(t, "preserve", g.Disposition)
	require.NotEqual(t, before.EvidenceHash, g.EvidenceHash)
	require.Nil(t, g.Solver)
	require.Nil(t, g.Snapshot)
	require.NotNil(t, g.Blob)
	require.Contains(t, g.Unverified, "solver_result_or_link_indexes")
	require.Contains(t, g.Unverified, "native_snapshot_records_or_allocation")
}

func TestDSGGraphNeedsNativeAllocations(t *testing.T) {
	f := dsgNewGraphFixture(t)
	require.NoError(t, os.Rename(f.allocation, f.allocation+"-moved"))
	require.NoError(t, os.Truncate(f.blobFile, 1))
	g := f.observe(t)
	require.Equal(t, "preserve", g.Disposition)
	require.True(t, g.Containerd.SnapshotMapping)
	require.Nil(t, g.Snapshot)
	require.Nil(t, g.Blob)
	require.Contains(t, g.Unverified, "native_snapshot_records_or_allocation")
	require.Contains(t, g.Unverified, "local_blob_metadata_or_allocation")
}
