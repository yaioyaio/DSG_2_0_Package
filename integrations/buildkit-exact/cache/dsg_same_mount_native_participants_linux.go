//go:build linux

package cache

import (
	"archive/tar"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/containerd/containerd/v2/plugins/content/local"
	"github.com/containerd/containerd/v2/plugins/snapshots/native"
	"github.com/opencontainers/go-digest"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const (
	reclaimSolver  = "solver.db"
	reclaimBackend = "containerd.db"
	reclaimCache   = "metadata_v2.db"
	reclaimNative  = "snapshots/metadata.db"
	reclaimHistory = "history.db"
)

var reclaimDatabases = [...]string{reclaimSolver, reclaimBackend, reclaimCache, reclaimHistory}

type dsgReclaimBlob struct {
	Digest digest.Digest
	Size   int64
}

type dsgReclaimLayout struct {
	Blobs                                [2]dsgReclaimBlob
	CacheIndexes                         []string
	Backing                              dsgDeletionLayout
	Files                                [len(reclaimDatabases)]dsgDeletionIdentity
	Snapshot, Native, NativeView         string
	ViewID                               uint64
	Graph, History, PeerRecord, PeerNode string
}

type dsgReclaimInput struct {
	Plan   dsgRecoveryPlan
	Layout dsgReclaimLayout
}

type dsgReclaimFixture struct {
	*dsgDeletionFixture
	shape dsgReclaimLayout
}

func (s *dsgReclaimFixture) fileIDs() (out [len(reclaimDatabases)]dsgDeletionIdentity, err error) {
	root, err := dsgAllocationStatAt(int(s.dir.Fd()), "")
	if err != nil {
		return out, err
	}
	for i, name := range reclaimDatabases {
		fd, err := unix.Openat2(int(s.dir.Fd()), name, &unix.OpenHow{Flags: unix.O_PATH | unix.O_CLOEXEC, Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS})
		if err != nil {
			return out, err
		}
		st, err := dsgAllocationStatAt(fd, "")
		unix.Close(fd)
		if err != nil || !st.ownedBy(root) || st.Mode != unix.S_IFREG|0600 || st.Links != 1 {
			return out, ErrDSGExactPreserve
		}
		out[i] = dsgDeletionIdentity{st.MountID, st.Inode, st.DeviceMajor, st.DeviceMinor}
	}
	return out, nil
}

func (s *dsgReclaimFixture) guard() error {
	ids, err := s.identities()
	if err != nil || ids != s.shape.Backing.Identities {
		return ErrDSGExactPreserve
	}
	files, err := s.fileIDs()
	if err != nil || files != s.shape.Files {
		return ErrDSGExactPreserve
	}
	_, err = s.names(".", append([]string{"snapshots", "content", dsgDeletionJournal}, reclaimDatabases[:]...), "")
	if err != nil {
		return err
	}
	if _, err = s.names("snapshots", []string{"snapshots", "metadata.db"}, ""); err != nil {
		return err
	}
	return s.withDB(reclaimHistory, true, func(tx *bolt.Tx) error {
		hash, err := dsgTrialTree(tx, func([]string) bool { return false })
		if err != nil || hash != s.shape.History {
			return ErrDSGExactPreserve
		}
		return nil
	})
}

func (s *dsgReclaimFixture) withDB(name string, readOnly bool, fn func(*bolt.Tx) error) error {
	db, err := bolt.Open(filepath.Join(s.root, name), 0600, &bolt.Options{ReadOnly: readOnly, Timeout: time.Second})
	if err != nil {
		return err
	}
	defer db.Close()
	if readOnly {
		return db.View(fn)
	}
	return db.Update(fn)
}

// Every root in this list is exact. In particular neither a suffix match nor a
// recursive scan is used to select mutable entries. A complete logical tree hash
// covers everything outside these selectors.
func (s *dsgReclaimFixture) selection(stage int) (string, [][]string) {
	id, node, blob := s.plan.Request.RecordID, s.plan.Objects[0].Key, s.plan.Objects[3].Key
	ns := dsgGraphNamespace
	switch stage {
	case 0:
		return reclaimSolver, [][]string{{"_result", node}, {"_byresult", s.plan.Request.Worker + "::" + id}, {"_links", node}}
	case 1:
		return reclaimBackend, [][]string{{"v1", ns, "leases", id}, {"v1", ns, "leases", id + "-variants"}, {"v1", ns, "snapshots", "native", s.shape.Snapshot + "-view"}, {"v1", ns, "snapshots", "native", s.shape.Snapshot}, {"v1", ns, "content", "blob", blob}}
	case 2:
		parent, _ := strconv.ParseUint(s.layout.TargetID, 10, 64)
		return reclaimNative, [][]string{{"v1", "snapshots", s.shape.NativeView}, {"v1", "parents", string(dsgGraphNativeIndexKey(parent, s.shape.ViewID))}}
	case 3:
		return reclaimNative, [][]string{{"v1", "snapshots", s.shape.Native}}
	case 5:
		paths := [][]string{{dsgMain, id}}
		for _, index := range s.shape.CacheIndexes {
			paths = append(paths, []string{"_index", index + "::" + id})
		}
		return reclaimCache, paths
	}
	return "", nil
}

func dsgReclaimParent(tx *bolt.Tx, path []string) (*bolt.Bucket, []byte, error) {
	b := tx.Bucket([]byte(path[0]))
	for _, key := range path[1 : len(path)-1] {
		if b == nil {
			return nil, nil, ErrDSGExactPreserve
		}
		b = b.Bucket([]byte(key))
	}
	if b == nil {
		return nil, nil, ErrDSGExactPreserve
	}
	return b, []byte(path[len(path)-1]), nil
}

func dsgReclaimContains(paths [][]string, path []string) bool {
	for _, owned := range paths {
		if slices.Equal(owned, path) {
			return true
		}
	}
	return false
}

func (s *dsgReclaimFixture) dbEvidence(tx *bolt.Tx, stage int) (target, peer string, err error) {
	_, owned := s.selection(stage)
	var other [][]string
	if stage == 2 {
		_, other = s.selection(3)
	}
	if stage == 3 {
		_, other = s.selection(2)
	}
	present := 0
	for _, path := range owned {
		b, key, err := dsgReclaimParent(tx, path)
		if err != nil {
			return "", "", err
		}
		if b.Bucket(key) != nil || b.Get(key) != nil {
			present++
		}
	}
	if present != 0 && present != len(owned) {
		return "", "", ErrDSGExactPreserve
	}
	peer, err = dsgTrialTree(tx, func(path []string) bool { return dsgReclaimContains(owned, path) || dsgReclaimContains(other, path) })
	if err != nil || present == 0 {
		return "", peer, err
	}
	target, err = dsgTrialTree(tx, func(path []string) bool { return dsgReclaimContains(other, path) })
	return target, peer, err
}

func (s *dsgReclaimFixture) blob(role string) ocispec.Descriptor {
	b := s.shape.Blobs[0]
	if role == "peer" {
		b = s.shape.Blobs[1]
	}
	return ocispec.Descriptor{Digest: b.Digest, Size: b.Size}
}

func (s *dsgReclaimFixture) snapshotPayload(ctx context.Context, id, role string) (string, error) {
	dir := []string{"snapshots", "snapshots", id}
	if _, err := s.names(filepath.Join(dir...), []string{dsgManifestlessFile}, ""); err != nil {
		return "", err
	}
	observed, err := dsgObserveAllocation(ctx, int(s.dir.Fd()), dir, nil, true, 0)
	if err != nil || observed.Entries != 2 || observed.AllocatedBlocks512 == 0 {
		return "", ErrDSGExactPreserve
	}
	data, err := s.payloadHash(ctx, append(dir, dsgManifestlessFile), s.payload("snapshot", role))
	return dsgDeletionHash([]string{observed.MetadataSHA256, data}), err
}

func (s *dsgReclaimFixture) layerHash(ctx context.Context, desc ocispec.Descriptor, role string) (string, error) {
	if !dsgGraphSHA256(desc.Digest.String()) || desc.Size < 1 || desc.Size > 65536 {
		return "", ErrDSGExactPreserve
	}
	parts := []string{"content", "blobs", "sha256", desc.Digest.Encoded()}
	fd, err := unix.Openat2(int(s.dir.Fd()), filepath.Join(parts...), &unix.OpenHow{
		Flags: unix.O_RDONLY | unix.O_CLOEXEC | unix.O_NOATIME | unix.O_NONBLOCK, Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
	})
	if err != nil {
		return "", err
	}
	f := os.NewFile(uintptr(fd), "private-generated-layer")
	defer f.Close()
	st, err := dsgAllocationStatAt(fd, "")
	if err != nil || st.Mode&unix.S_IFMT != unix.S_IFREG || st.Links != 1 || st.Size != uint64(desc.Size) {
		return "", ErrDSGExactPreserve
	}
	raw, err := io.ReadAll(io.LimitReader(f, desc.Size+1))
	if err != nil || int64(len(raw)) != desc.Size || digest.FromBytes(raw) != desc.Digest || !dsgAllocationMatches(fd, "", st) {
		return "", ErrDSGExactPreserve
	}
	archive := tar.NewReader(bytes.NewReader(raw))
	entry, err := archive.Next()
	payload := s.payload("snapshot", role)
	if err != nil || entry.Typeflag != tar.TypeReg || strings.TrimPrefix(entry.Name, "./") != dsgManifestlessFile ||
		entry.Size != int64(len(payload)) {
		return "", ErrDSGExactPreserve
	}
	data, err := io.ReadAll(io.LimitReader(archive, int64(len(payload))+1))
	if err != nil || !bytes.Equal(data, []byte(payload)) {
		return "", ErrDSGExactPreserve
	}
	if _, err = archive.Next(); !errors.Is(err, io.EOF) {
		return "", ErrDSGExactPreserve
	}
	alloc, err := dsgObserveAllocation(ctx, int(s.dir.Fd()), parts, nil, false, uint64(desc.Size))
	if err != nil || alloc.RootInode != st.Inode || alloc.Entries != 1 || alloc.AllocatedBlocks512 == 0 {
		return "", ErrDSGExactPreserve
	}
	return dsgDeletionHash([]string{alloc.MetadataSHA256, desc.Digest.String(), fmt.Sprint(desc.Size)}), nil
}

func (s *dsgReclaimFixture) content(ctx context.Context) (target, peer string, err error) {
	for _, inventory := range []struct {
		dir   string
		names []string
	}{
		{"content", []string{"blobs", "ingest"}}, {"content/ingest", nil}, {"content/blobs", []string{"sha256"}},
	} {
		if _, err = s.names(inventory.dir, inventory.names, ""); err != nil {
			return
		}
	}
	targetBlob, peerBlob := s.blob("target"), s.blob("peer")
	exists, err := s.names("content/blobs/sha256", []string{peerBlob.Digest.Encoded()}, targetBlob.Digest.Encoded())
	if err != nil {
		return "", "", err
	}
	peer, err = s.layerHash(ctx, peerBlob, "peer")
	if err != nil || !exists {
		return "", peer, err
	}
	target, err = s.layerHash(ctx, targetBlob, "target")
	return
}

func (s *dsgReclaimFixture) evidence(ctx context.Context, stage int) (target, peer string, err error) {
	if err = s.guard(); err != nil {
		return
	}
	if stage == 4 {
		return s.content(ctx)
	}
	name, _ := s.selection(stage)
	err = s.withDB(name, true, func(tx *bolt.Tx) error {
		var readErr error
		target, peer, readErr = s.dbEvidence(tx, stage)
		return readErr
	})
	if err != nil || (stage != 2 && stage != 3) {
		return
	}
	exists, err := s.names("snapshots/snapshots", []string{s.layout.PeerID}, s.layout.TargetID)
	if err != nil {
		return "", "", err
	}
	peerFile, err := s.snapshotPayload(ctx, s.layout.PeerID, "peer")
	if err != nil {
		return "", "", err
	}
	peer = dsgDeletionHash([]string{peer, peerFile})
	if stage == 3 {
		if exists != (target != "") {
			return "", "", ErrDSGExactPreserve
		}
		if exists {
			file, err := s.snapshotPayload(ctx, s.layout.TargetID, "target")
			if err != nil {
				return "", "", err
			}
			target = dsgDeletionHash([]string{target, file})
		}
	}
	return target, peer, nil
}

func (s *dsgReclaimFixture) observe(ctx context.Context, p dsgRecoveryPlan, stage int) (dsgRecoveryObservation, error) {
	if p != s.plan || !(p.valid(dsgNativeReclamationQualification) || p.valid(dsgNativeSameMount)) || stage < 0 || stage >= len(p.stages()) {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	target, peer, err := s.evidence(ctx, stage)
	if err != nil || peer != p.object(stage).NonTargetSHA256 {
		return dsgRecoveryUnknown, ErrDSGExactPreserve
	}
	marked, err := s.callMarker(p, stage)
	if err != nil {
		return dsgRecoveryUnknown, err
	}
	if !marked && target == p.object(stage).BeforeSHA256 {
		return dsgRecoveryBefore, nil
	}
	if marked && target == "" {
		return dsgRecoveryAfter, nil
	}
	return dsgRecoveryUnknown, ErrDSGExactPreserve
}

func (s *dsgReclaimFixture) commit(ctx context.Context, p dsgRecoveryPlan, stage int) error {
	state, err := s.observe(ctx, p, stage)
	if err != nil || state != dsgRecoveryBefore {
		return ErrDSGExactPreserve
	}
	if err = s.recordCall(ctx, p, stage); err != nil {
		return err
	}
	if !dsgRecoveryAuthorized(ctx, p) {
		return ErrDSGExactPreserve
	}
	s.calls++
	switch stage {
	case 0, 1, 5:
		name, owned := s.selection(stage)
		return s.withDB(name, false, func(tx *bolt.Tx) error {
			before, peer, err := s.dbEvidence(tx, stage)
			if err != nil || before != p.object(stage).BeforeSHA256 || peer != p.object(stage).NonTargetSHA256 || !dsgRecoveryAuthorized(ctx, p) {
				return ErrDSGExactPreserve
			}
			for _, path := range owned {
				b, key, err := dsgReclaimParent(tx, path)
				if err != nil {
					return err
				}
				if b.Bucket(key) != nil {
					err = b.DeleteBucket(key)
				} else {
					err = b.Delete(key)
				}
				if err != nil {
					return err
				}
			}
			return nil
		})
	case 2, 3:
		sn, err := native.NewSnapshotter(filepath.Join(s.root, "snapshots"))
		if err != nil {
			return err
		}
		key := s.shape.Native
		if stage == 2 {
			key = s.shape.NativeView
		}
		err = sn.Remove(ctx, key)
		closeErr := sn.Close()
		if err != nil {
			return err
		}
		if closeErr != nil {
			return closeErr
		}
		return s.syncDir("snapshots/snapshots")
	case 4:
		cs, err := local.NewStore(filepath.Join(s.root, "content"))
		if err != nil {
			return err
		}
		if err = cs.Delete(ctx, s.blob("target").Digest); err != nil {
			return err
		}
		return s.syncDir("content/blobs/sha256")
	}
	return ErrDSGExactPreserve
}

func (s *dsgReclaimFixture) syncDir(rel string) error {
	d, err := os.Open(filepath.Join(s.root, rel))
	if err != nil {
		return err
	}
	defer d.Close()
	return d.Sync()
}
