//go:build linux

package cache

import (
	"archive/tar"
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/pkg/namespaces"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/moby/buildkit/cache/config"
	"github.com/moby/buildkit/identity"
	"github.com/moby/buildkit/snapshot"
	containerdsnapshot "github.com/moby/buildkit/snapshot/containerd"
	"github.com/moby/buildkit/solver"
	"github.com/moby/buildkit/solver/bboltcachestorage"
	"github.com/moby/buildkit/solver/llbsolver/history"
	"github.com/moby/buildkit/util/compression"
	"github.com/moby/buildkit/util/leaseutil"
	digest "github.com/opencontainers/go-digest"
	ocispecs "github.com/opencontainers/image-spec/specs-go/v1"
	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

func dsgManifestlessLayer(t *testing.T, ctx context.Context, ref ImmutableRef, payload []byte) (ocispecs.Descriptor, []byte) {
	t.Helper()
	remotes, err := ref.GetRemotes(ctx, true, config.RefConfig{Compression: compression.New(compression.Uncompressed)}, false, nil)
	require.NoError(t, err)
	require.Len(t, remotes, 1)
	require.Len(t, remotes[0].Descriptors, 1)
	desc := remotes[0].Descriptors[0]
	require.True(t, dsgGraphSHA256(desc.Digest.String()) && desc.Size > 0 && desc.Size <= 65536)
	layer, err := content.ReadBlob(ctx, remotes[0].Provider, desc)
	require.NoError(t, err)
	require.Equal(t, desc.Size, int64(len(layer)))
	require.Equal(t, desc.Digest, digest.FromBytes(layer))
	archive := tar.NewReader(bytes.NewReader(layer))
	entry, err := archive.Next()
	require.NoError(t, err)
	require.Equal(t, dsgManifestlessFile, strings.TrimPrefix(entry.Name, "./"))
	require.Equal(t, byte(tar.TypeReg), entry.Typeflag)
	require.Equal(t, int64(len(payload)), entry.Size)
	read, err := io.ReadAll(io.LimitReader(archive, int64(len(payload)+1)))
	require.NoError(t, err)
	require.True(t, bytes.Equal(payload, read))
	_, err = archive.Next()
	require.ErrorIs(t, err, io.EOF)
	return desc, layer
}

type dsgManifestlessRecord struct {
	RecordID, SnapshotID, AllocationID, AllocationRoot string
	Layer                                              ocispecs.Descriptor
	LayerBytes                                         []byte
}

// Create a scratch record with the pinned cache manager and capture the actual
// native allocation and exported bytes before closing its originating worker.
func dsgCreateManifestlessRecord(t *testing.T, ctx context.Context, co *cmOut, payload []byte) dsgManifestlessRecord {
	t.Helper()
	prior, err := co.lm.List(ctx)
	require.NoError(t, err)
	expected := make(map[string]bool, len(prior)+2)
	for _, l := range prior {
		expected[l.ID] = true
	}
	mut, err := co.manager.New(ctx, nil, nil, CachePolicyDefault)
	require.NoError(t, err)
	mounts, err := co.testSnapshotter.Mounts(ctx, mut.ID())
	require.NoError(t, err)
	require.Len(t, mounts, 1)
	allocation := mounts[0].Source
	require.True(t, filepath.IsAbs(allocation) && filepath.Clean(allocation) == allocation)
	allocationID := filepath.Base(allocation)
	require.Regexp(t, `^[1-9][0-9]{0,2}$`, allocationID)
	mountable, err := mut.Mount(ctx, false, nil)
	require.NoError(t, err)
	mount := snapshot.LocalMounter(mountable)
	dir, err := mount.Mount()
	require.NoError(t, err)
	writeErr := os.WriteFile(filepath.Join(dir, dsgManifestlessFile), payload, 0600)
	require.NoError(t, mount.Unmount())
	require.NoError(t, writeErr)
	ref, err := mut.Commit(ctx)
	require.NoError(t, err)
	recordID := ref.ID()
	layer, layerBytes := dsgManifestlessLayer(t, ctx, ref, payload)
	snapshotID := ref.(*immutableRef).getSnapshotID()
	require.NoError(t, ref.Release(ctx))
	// Finalize removes the old mutable record asynchronously and GetRemotes
	// discards its staging lease asynchronously. Let those native writers
	// finish before closing their stores, without deleting anything here.
	expected[recordID], expected[recordID+"-variants"] = true, true
	manager := co.manager.(*cacheManager)
	require.Eventually(t, func() bool {
		manager.mu.Lock()
		_, pending := manager.records[mut.ID()]
		manager.mu.Unlock()
		if pending {
			return false
		}
		leases, err := co.lm.List(ctx)
		if err != nil || len(leases) != len(expected) {
			return false
		}
		for _, l := range leases {
			if !expected[l.ID] {
				return false
			}
		}
		return true
	}, 2*time.Second, 10*time.Millisecond)
	return dsgManifestlessRecord{RecordID: recordID, SnapshotID: snapshotID, AllocationID: allocationID,
		AllocationRoot: allocation, Layer: layer, LayerBytes: layerBytes}
}

// The caller has just created or exclusively enrolled an empty private directory.
// All records, content and the single solver association originate in this process.
func dsgProduceManifestless(t *testing.T, root string) {
	t.Helper()
	ctx := namespaces.WithNamespace(t.Context(), dsgGraphNamespace)
	workerID := identity.NewID()
	payload := []byte("DSG manifestless native candidate " + workerID + "\n")
	worker := filepath.Join(root, "runc-native")
	require.NoError(t, os.Mkdir(worker, 0700))
	require.NoError(t, os.WriteFile(filepath.Join(worker, "workerid"), []byte(workerID), 0600))
	contentDir := filepath.Join(worker, "content")
	require.NoError(t, os.Mkdir(contentDir, 0700))
	sn, err := native.NewSnapshotter(filepath.Join(worker, "snapshots"))
	require.NoError(t, err)
	co, closeWorker, err := newCacheManager(ctx, t, cmOpt{tmpdir: contentDir, snapshotter: sn, snapshotterName: "native"})
	require.NoError(t, err)

	// Use the pinned queue's initialization for the actual empty history store. Its
	// background housekeeping and the enrollment FD share this test child's lifetime.
	hdb, err := bolt.Open(filepath.Join(root, "history.db"), 0600, nil)
	require.NoError(t, err)
	stop := make(chan struct{})
	_, err = history.NewQueue(history.QueueOpt{DB: hdb, ContentStore: co.cs.(*containerdsnapshot.Store), LeaseManager: co.lm.(*leaseutil.Manager), GracefulStop: stop})
	require.NoError(t, err)
	generated := dsgCreateManifestlessRecord(t, ctx, co, payload)
	require.Equal(t, filepath.Join(worker, "snapshots", "snapshots", generated.AllocationID), generated.AllocationRoot)
	recordID, layer := generated.RecordID, generated.Layer.Digest

	// Save one parentless result through the real bbolt index API. This test
	// produces a synthetic association directly, without a solve or image export.
	store, err := bboltcachestorage.NewStore(filepath.Join(root, "cache.db"))
	require.NoError(t, err)
	origin := digest.FromBytes(append([]byte("manifestless-native-payload-v1\x00"), payload...))
	require.NoError(t, store.AddResult(origin.String(), solver.CacheResult{ID: workerID + "::" + recordID, CreatedAt: time.Now().UTC()}))
	require.NoError(t, store.Close())
	close(stop)
	require.NoError(t, hdb.Close())
	closeWorker()
	require.NoError(t, sn.Close())
	// The upstream test manager puts these two DB files in its content root.
	// Place the closed stores at the standard runc worker locations before inspecting.
	for from, to := range map[string]string{"metadata.db": "metadata_v2.db", "containerdmeta.db": "containerdmeta.db"} {
		require.NoError(t, os.Rename(filepath.Join(contentDir, from), filepath.Join(worker, to)))
	}
	rootDir, err := os.Open(root)
	require.NoError(t, err)
	defer rootDir.Close()
	observation, err := dsgInspectPinned(int(rootDir.Fd()), recordID)
	require.NoError(t, err)
	g := observation.Graph
	require.NotNil(t, g)
	require.Equal(t, []string{"native_allocation_contents"}, g.Unverified)
	require.True(t, g.Ownership.Checked && g.Ownership.Exclusive, "ownership %s", g.Ownership.Reason)
	require.Equal(t, "", g.Ownership.IncomingContentKeySHA256)
	require.Equal(t, 1, g.Solver.ResultNodes)
	require.Equal(t, 0, g.Solver.ParentLinks+g.Solver.ChildLinks)
	require.Equal(t, layer.Encoded(), g.Allocations.Blob.DeclaredSHA256)
	require.Equal(t, 2, g.Allocations.Snapshot.Entries)
	// A fresh content store contains precisely the exported layer. In particular
	// an accidentally generated index, manifest or config is a hard failure.
	blobs, err := os.ReadDir(filepath.Join(contentDir, "blobs", "sha256"))
	require.NoError(t, err)
	require.Len(t, blobs, 1)
	require.Equal(t, layer.Encoded(), blobs[0].Name())
	require.True(t, blobs[0].Type().IsRegular())
	t.Logf("DSG_MANIFESTLESS_PRODUCED %s %s %s %s", workerID, recordID, layer.Encoded(), digest.FromBytes(payload).Encoded())
}

func TestDSGNativeManifestlessCandidate(t *testing.T) {
	args := os.Args
	if len(args) >= 3 && args[len(args)-3] == "--" && args[len(args)-2] == "dsg-seal-manifestless" {
		root := args[len(args)-1]
		require.NoError(t, DSGAcquireQuiescentDaemon(root))
		dsgProduceManifestless(t, root)
		return
	}
	if args[len(args)-1] == "dsg-private-manifestless" {
		parent, err := os.Readlink("/proc/self/fd/3")
		require.NoError(t, err)
		root := filepath.Join(parent, "fresh-manifestless")
		require.NoError(t, os.Mkdir(root, 0700))
		dsgProduceManifestless(t, root)
		return
	}
	parent := t.TempDir()
	dir, err := os.Open(parent)
	require.NoError(t, err)
	defer dir.Close()
	exe, err := os.Executable()
	require.NoError(t, err)
	ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, exe, "-test.run=^TestDSGNativeManifestlessCandidate$", "-test.v", "--", "dsg-private-manifestless")
	cmd.Env = []string{"TMPDIR=" + parent}
	cmd.ExtraFiles = []*os.File{dir}
	out, err := cmd.CombinedOutput()
	if err != nil {
		locations := regexp.MustCompile(`dsg_manifestless_candidate_linux_test\.go:\d+`).FindAll(out, 5)
		t.Fatalf("manifestless child failed (%s), timeout=%t", locations, errors.Is(ctx.Err(), context.DeadlineExceeded))
	}
	proof := regexp.MustCompile(`(?m)\bDSG_MANIFESTLESS_PRODUCED [a-z0-9]{25} [a-z0-9]{25} [0-9a-f]{64} [0-9a-f]{64}$`).FindAll(out, 2)
	require.Len(t, proof, 1)
	t.Log(string(proof[0]))
}
