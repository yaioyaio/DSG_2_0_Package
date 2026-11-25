//go:build linux

package cache

import (
	"context"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/containerd/containerd/v2/core/content"
	"github.com/containerd/containerd/v2/core/snapshots"
	"github.com/containerd/containerd/v2/core/snapshots/storage"
	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/containerd/errdefs"
	"github.com/opencontainers/go-digest"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	"github.com/stretchr/testify/require"
	"golang.org/x/sys/unix"
)

const (
	dsgBackingDir        = "fresh-native-backing"
	dsgBackingTarget     = "trial-target"
	dsgBackingActive     = "trial-active"
	dsgBackingPeer       = "trial-peer"
	dsgBackingPreimage   = "preimage.json"
	dsgBackingProofLabel = "dsg.trial.snapshot-proof"
	dsgBackingTargetData = "generated exact target backing payload"
	dsgBackingPeerData   = "generated untargeted peer backing payload"
)

type dsgBackingFile struct {
	Digest string
	Size   int64
	Device uint64
	Inode  uint64
	Mode   uint32
	MTime  unix.Timespec
}

type dsgBackingPreimageEvidence struct {
	Domain               string
	TargetID, PeerID     string
	TargetFile, PeerFile dsgBackingFile
	PeerSnapshot         snapshots.Info
}

func dsgBackingEvidence(t *testing.T, path, expected string) dsgBackingFile {
	t.Helper()
	var st unix.Stat_t
	require.NoError(t, unix.Lstat(path, &st))
	require.Equal(t, uint32(unix.S_IFREG), st.Mode&unix.S_IFMT)
	require.Equal(t, int64(len(expected)), st.Size)
	f, err := os.Open(path)
	require.NoError(t, err)
	raw, err := io.ReadAll(io.LimitReader(f, st.Size+1))
	require.NoError(t, f.Close())
	require.NoError(t, err)
	require.Equal(t, digest.FromString(expected), digest.FromBytes(raw))
	return dsgBackingFile{digest.FromBytes(raw).String(), st.Size, uint64(st.Dev), st.Ino, st.Mode, st.Mtim}
}

func dsgBackingNames(t *testing.T, dir string) []string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	require.NoError(t, err)
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return names
}

func dsgBackingWrite(t *testing.T, path string, data []byte) {
	t.Helper()
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	require.NoError(t, err)
	_, err = f.Write(data)
	require.NoError(t, err)
	require.NoError(t, f.Sync())
	require.NoError(t, f.Close())
}

func dsgBackingSavePreimage(t *testing.T, root string, pre dsgBackingPreimageEvidence) string {
	t.Helper()
	raw, err := json.Marshal(pre)
	require.NoError(t, err)
	dsgBackingWrite(t, filepath.Join(root, dsgBackingPreimage), raw)
	return dsgRecoveryHash(raw)
}

func dsgBackingPrepare(t *testing.T, ctx context.Context, sn snapshots.Snapshotter, root, key, parent string) string {
	t.Helper()
	mounts, err := sn.Prepare(ctx, key, parent)
	require.NoError(t, err)
	require.Len(t, mounts, 1)
	source := mounts[0].Source
	require.Equal(t, filepath.Join(root, "snapshots", "snapshots"), filepath.Dir(source))
	id := filepath.Base(source)
	require.Regexp(t, `^[1-9][0-9]{0,2}$`, id)
	return id
}

func dsgBackingSnapshotPath(root, id, leaf string) string {
	return filepath.Join(root, "snapshots", "snapshots", id, leaf)
}

func dsgBackingCommitSnapshot(t *testing.T, root string) {
	t.Helper()
	ctx := t.Context()
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	peer := dsgBackingPrepare(t, ctx, sn, root, dsgBackingPeer+"-active", "")
	peerPath := dsgBackingSnapshotPath(root, peer, "peer-data")
	dsgBackingWrite(t, peerPath, []byte(dsgBackingPeerData))
	require.NoError(t, sn.Commit(ctx, dsgBackingPeer, dsgBackingPeer+"-active"))
	peerInfo, err := sn.Stat(ctx, dsgBackingPeer)
	require.NoError(t, err)
	target := dsgBackingPrepare(t, ctx, sn, root, dsgBackingActive, dsgBackingPeer)
	targetPath := dsgBackingSnapshotPath(root, target, "target-data")
	dsgBackingWrite(t, targetPath, []byte(dsgBackingTargetData))
	active, err := sn.Stat(ctx, dsgBackingActive)
	require.NoError(t, err)
	require.Equal(t, snapshots.KindActive, active.Kind)
	_, err = sn.Stat(ctx, dsgBackingTarget)
	require.ErrorIs(t, err, errdefs.ErrNotFound)
	proof := dsgBackingSavePreimage(t, root, dsgBackingPreimageEvidence{
		Domain: "snapshot", TargetID: target, PeerID: peer,
		TargetFile: dsgBackingEvidence(t, targetPath, dsgBackingTargetData),
		PeerFile:   dsgBackingEvidence(t, peerPath, dsgBackingPeerData), PeerSnapshot: peerInfo,
	})
	require.NoError(t, sn.Commit(ctx, dsgBackingTarget, dsgBackingActive,
		snapshots.WithLabels(map[string]string{dsgBackingProofLabel: proof})))
	// Leave the snapshotter open so no clean shutdown is part of the proof.
	os.Exit(86)
}

func dsgBackingVerifySnapshot(t *testing.T, root string, pre dsgBackingPreimageEvidence, proof string) {
	t.Helper()
	ctx := t.Context()
	require.Equal(t, "snapshot", pre.Domain)
	for _, id := range []string{pre.TargetID, pre.PeerID} {
		require.Regexp(t, `^[1-9][0-9]{0,2}$`, id)
	}
	require.NotEqual(t, pre.TargetID, pre.PeerID)
	metaPath := filepath.Join(root, "snapshots", "metadata.db")
	ms, err := storage.NewMetaStore(metaPath)
	require.NoError(t, err)
	require.NoError(t, ms.WithTransaction(ctx, false, func(ctx context.Context) error {
		id, target, use, err := storage.GetInfo(ctx, dsgBackingTarget)
		require.NoError(t, err)
		require.Equal(t, pre.TargetID, id)
		require.Equal(t, snapshots.KindCommitted, target.Kind)
		require.Equal(t, dsgBackingPeer, target.Parent)
		require.Equal(t, map[string]string{dsgBackingProofLabel: proof}, target.Labels)
		require.Positive(t, use.Inodes)
		id, peer, _, err := storage.GetInfo(ctx, dsgBackingPeer)
		require.NoError(t, err)
		require.Equal(t, pre.PeerID, id)
		require.Equal(t, pre.PeerSnapshot, peer)
		return nil
	}))
	require.NoError(t, ms.Close())
	require.Equal(t, pre.TargetFile, dsgBackingEvidence(t, dsgBackingSnapshotPath(root, pre.TargetID, "target-data"), dsgBackingTargetData))
	require.Equal(t, pre.PeerFile, dsgBackingEvidence(t, dsgBackingSnapshotPath(root, pre.PeerID, "peer-data"), dsgBackingPeerData))
	dsgBackingEvidence(t, dsgBackingSnapshotPath(root, pre.TargetID, "peer-data"), dsgBackingPeerData)
	require.ElementsMatch(t, []string{pre.PeerID, pre.TargetID}, dsgBackingNames(t, filepath.Join(root, "snapshots", "snapshots")))
	sn, err := native.NewSnapshotter(filepath.Join(root, "snapshots"))
	require.NoError(t, err)
	var names []string
	require.NoError(t, sn.Walk(ctx, func(_ context.Context, info snapshots.Info) error {
		names = append(names, info.Name)
		return nil
	}))
	require.ElementsMatch(t, []string{dsgBackingPeer, dsgBackingTarget}, names)
	before, err := dsgTrialAllocations(root, 2)
	require.NoError(t, err)
	// The exact commit cannot transition the already-consumed active key again.
	require.ErrorIs(t, sn.Commit(ctx, dsgBackingTarget, dsgBackingActive), errdefs.ErrNotFound)
	_, err = sn.Stat(ctx, dsgBackingActive)
	require.ErrorIs(t, err, errdefs.ErrNotFound)
	require.NoError(t, sn.Close())
	after, err := dsgTrialAllocations(root, 2)
	require.NoError(t, err)
	require.Equal(t, before, after)
	require.Equal(t, []string{dsgBackingPreimage, "snapshots"}, dsgBackingNames(t, root))
}

func dsgBackingDescriptor(data string) ocispec.Descriptor {
	return ocispec.Descriptor{Digest: digest.FromString(data), Size: int64(len(data))}
}

func dsgBackingBlobPath(root string, desc ocispec.Descriptor) string {
	return filepath.Join(root, "blobs", desc.Digest.Algorithm().String(), desc.Digest.Encoded())
}

func dsgBackingCommitBlob(t *testing.T, root string) {
	t.Helper()
	ctx := t.Context()
	cs, err := local.NewStore(root)
	require.NoError(t, err)
	peer, target := dsgBackingDescriptor(dsgBackingPeerData), dsgBackingDescriptor(dsgBackingTargetData)
	require.NoError(t, content.WriteBlob(ctx, cs, dsgBackingPeer, strings.NewReader(dsgBackingPeerData), peer))
	_, err = cs.Info(ctx, target.Digest)
	require.ErrorIs(t, err, errdefs.ErrNotFound)
	dsgBackingSavePreimage(t, root, dsgBackingPreimageEvidence{
		Domain: "blob", TargetID: target.Digest.String(), PeerID: peer.Digest.String(),
		PeerFile: dsgBackingEvidence(t, dsgBackingBlobPath(root, peer), dsgBackingPeerData),
	})
	require.NoError(t, content.WriteBlob(ctx, cs, dsgBackingTarget, strings.NewReader(dsgBackingTargetData), target))
	os.Exit(86)
}

type dsgBackingNoRead struct{ reads int }

func (r *dsgBackingNoRead) Read([]byte) (int, error) {
	r.reads++
	return 0, ErrDSGExactPreserve
}

func dsgBackingVerifyBlob(t *testing.T, root string, pre dsgBackingPreimageEvidence) {
	t.Helper()
	ctx := t.Context()
	require.Equal(t, "blob", pre.Domain)
	peer, target := dsgBackingDescriptor(dsgBackingPeerData), dsgBackingDescriptor(dsgBackingTargetData)
	require.Equal(t, target.Digest.String(), pre.TargetID)
	require.Equal(t, peer.Digest.String(), pre.PeerID)
	cs, err := local.NewStore(root)
	require.NoError(t, err)
	var digests []digest.Digest
	require.NoError(t, cs.Walk(ctx, func(info content.Info) error {
		digests = append(digests, info.Digest)
		return nil
	}))
	require.ElementsMatch(t, []digest.Digest{peer.Digest, target.Digest}, digests)
	for _, desc := range []ocispec.Descriptor{peer, target} {
		info, err := cs.Info(ctx, desc.Digest)
		require.NoError(t, err)
		require.Equal(t, desc.Digest, info.Digest)
		require.Equal(t, desc.Size, info.Size)
		raw, err := content.ReadBlob(ctx, cs, desc)
		require.NoError(t, err)
		require.Equal(t, desc.Digest, digest.FromBytes(raw))
	}
	peerPath, targetPath := dsgBackingBlobPath(root, peer), dsgBackingBlobPath(root, target)
	require.Equal(t, pre.PeerFile, dsgBackingEvidence(t, peerPath, dsgBackingPeerData))
	committed := dsgBackingEvidence(t, targetPath, dsgBackingTargetData)
	replay := &dsgBackingNoRead{}
	require.NoError(t, content.WriteBlob(ctx, cs, dsgBackingTarget, replay, target))
	require.Zero(t, replay.reads)
	require.Equal(t, committed, dsgBackingEvidence(t, targetPath, dsgBackingTargetData))
	require.Equal(t, pre.PeerFile, dsgBackingEvidence(t, peerPath, dsgBackingPeerData))
	statuses, err := cs.ListStatuses(ctx)
	require.NoError(t, err)
	require.Empty(t, statuses)
	require.Equal(t, []string{"blobs", "ingest", dsgBackingPreimage}, dsgBackingNames(t, root))
}

func TestDSGNativeBackingRecovery(t *testing.T) {
	args := os.Args
	if len(args) > 2 && args[len(args)-2] == "dsg-new-native-backing" {
		parent, err := os.Readlink("/proc/self/fd/3")
		require.NoError(t, err)
		root := filepath.Join(parent, dsgBackingDir)
		require.NoError(t, os.Mkdir(root, 0700))
		switch args[len(args)-1] {
		case "snapshot":
			dsgBackingCommitSnapshot(t, root)
		case "blob":
			dsgBackingCommitBlob(t, root)
		default:
			t.Fatal("unknown native backing domain")
		}
	}
	for _, domain := range []string{"snapshot", "blob"} {
		t.Run(domain, func(t *testing.T) {
			parent := t.TempDir()
			d, err := os.Open(parent)
			require.NoError(t, err)
			defer d.Close()
			exe, err := os.Executable()
			require.NoError(t, err)
			ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
			defer cancel()
			cmd := exec.CommandContext(ctx, exe, "-test.run=^TestDSGNativeBackingRecovery$", "--", "dsg-new-native-backing", domain)
			cmd.Env = []string{"TMPDIR=" + parent}
			cmd.ExtraFiles = []*os.File{d}
			out, err := cmd.CombinedOutput()
			require.NotNil(t, cmd.ProcessState)
			if cmd.ProcessState.ExitCode() != 86 {
				lines := regexp.MustCompile(`dsg_native_backing_recovery_linux_test\.go:[0-9]+`).FindAll(out, 6)
				t.Fatalf("backing child failed: %v (%s)", err, lines)
			}
			root := filepath.Join(parent, dsgBackingDir)
			raw, err := os.ReadFile(filepath.Join(root, dsgBackingPreimage))
			require.NoError(t, err)
			var pre dsgBackingPreimageEvidence
			require.NoError(t, json.Unmarshal(raw, &pre))
			if domain == "snapshot" {
				dsgBackingVerifySnapshot(t, root, pre, dsgRecoveryHash(raw))
			} else {
				dsgBackingVerifyBlob(t, root, pre)
			}
		})
	}
}
