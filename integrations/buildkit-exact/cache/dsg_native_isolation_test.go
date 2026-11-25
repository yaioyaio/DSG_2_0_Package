package cache

import (
	"crypto/sha256"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/leases"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/stretchr/testify/require"
)

func TestDSGExactNativeIsolation(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("requires the Linux native snapshotter")
	}
	ctx := namespaces.WithNamespace(t.Context(), "dsg-native-test")
	newWorker := func(root string) *cmOut {
		t.Helper()
		snapshotter, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
		require.NoError(t, err)
		t.Cleanup(func() { require.NoError(t, snapshotter.Close()) })
		worker, closeWorker, err := newCacheManager(ctx, t, cmOpt{
			tmpdir: root, snapshotter: snapshotter, snapshotterName: "native",
		})
		require.NoError(t, err)
		t.Cleanup(closeWorker)
		return worker
	}
	firstRoot, secondRoot := t.TempDir(), t.TempDir()
	first, second := newWorker(firstRoot), newWorker(secondRoot)

	mutable, err := first.manager.New(ctx, nil, nil, CachePolicyRetain)
	require.NoError(t, err)
	target, err := mutable.Commit(ctx)
	require.NoError(t, err)
	sameWorker, err := first.manager.New(ctx, nil, nil, CachePolicyRetain)
	require.NoError(t, err)
	otherWorker, err := second.manager.New(ctx, nil, nil, CachePolicyRetain)
	require.NoError(t, err)

	metadataHashes := func(root string) [3][sha256.Size]byte {
		t.Helper()
		var hashes [3][sha256.Size]byte
		for i, path := range []string{"metadata.db", "containerdmeta.db", "snapshots/metadata.db"} {
			data, err := os.ReadFile(filepath.Join(root, path))
			require.NoError(t, err)
			hashes[i] = sha256.Sum256(data)
		}
		return hashes
	}
	firstBefore, secondBefore := metadataHashes(firstRoot), metadataHashes(secondRoot)

	observed, err := DSGInspectExact(ctx, first.manager, target.ID())
	require.NoError(t, err)
	require.Equal(t, target.ID(), observed.RecordID)
	require.Greater(t, observed.Aliases, 0, "a lazily committed native snapshot has a mutable alias")
	for _, peer := range []struct {
		worker *cmOut
		id     string
	}{{first, sameWorker.ID()}, {second, otherWorker.ID()}} {
		observed, err := DSGInspectExact(ctx, peer.worker.manager, peer.id)
		require.NoError(t, err)
		require.Equal(t, peer.id, observed.RecordID)
		require.Equal(t, peer.id, observed.SnapshotID)
		backing, err := DSGInspectExactBacking(ctx, peer.worker.manager, peer.id)
		require.NoError(t, err)
		require.True(t, backing.SnapshotPresent)
		require.True(t, backing.RecordLeasePresent)
		require.False(t, backing.BlobPresent)
		_, err = peer.worker.testSnapshotter.Stat(ctx, peer.id)
		require.NoError(t, err)
		resources, err := peer.worker.lm.ListResources(ctx, leases.Lease{ID: peer.id})
		require.NoError(t, err)
		require.Equal(t, []leases.Resource{{ID: peer.id, Type: "snapshots/native"}}, resources)
	}

	_, err = DSGInspectExact(ctx, second.manager, target.ID())
	require.Error(t, err, "the same selector has no authority in another worker")
	_, err = DSGInspectExact(ctx, first.manager, strings.Repeat("0", len(target.ID())))
	require.Error(t, err, "a full canonical selector must exist")

	api, err := NewDSGExactAPI(first.manager)
	require.NoError(t, err)
	now := time.Now()
	intent := DSGExactIntent{
		Project: strings.Repeat("a", 64), Worker: strings.Repeat("w", 25),
		Generation: strings.Repeat("b", 64), RecordID: target.ID(), Origin: strings.Repeat("c", 64),
		MetadataDigest: strings.Repeat("d", 64), Approval: strings.Repeat("e", 64), Intent: strings.Repeat("f", 64),
		ApprovedAtMS: now.UnixMilli(), ExpiresAtMS: now.Add(time.Minute).UnixMilli(),
	}
	require.True(t, intent.valid())
	_, err = api.Apply(ctx, intent)
	require.ErrorIs(t, err, ErrDSGExactPreserve, "a stock native worker cannot accept an apply")
	_, err = api.Receipt(intent)
	require.ErrorIs(t, err, ErrDSGExactPreserve, "no change or receipt can be attributed to the request")
	require.Equal(t, firstBefore, metadataHashes(firstRoot))
	require.Equal(t, secondBefore, metadataHashes(secondRoot))
	t.Log("native snapshotter: alias found, stock apply denied, two worker stores and leases unchanged, backend mutation calls=0")
}
