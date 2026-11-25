//go:build linux

package cache

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"time"

	"github.com/containerd/containerd/v2/core/metadata/boltutil"
	"github.com/containerd/containerd/v2/core/snapshots"
	ocispec "github.com/opencontainers/image-spec/specs-go/v1"
	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const dsgDeletionRoot = "fresh-native-deletion"
const dsgDeletionJournal = "deletion-qualification.db"
const dsgDeletionFixtureLabel = "dsg.deletion.fixture"

type dsgDeletionIdentity struct {
	Mount, Inode uint64
	Major, Minor uint32
}

var dsgDeletionPaths = [...]struct {
	rel  string
	mode uint16
}{
	{"", unix.S_IFDIR}, {"snapshots", unix.S_IFDIR}, {"snapshots/snapshots", unix.S_IFDIR},
	{"content", unix.S_IFDIR}, {"content/blobs", unix.S_IFDIR}, {"content/blobs/sha256", unix.S_IFDIR},
	{"content/ingest", unix.S_IFDIR}, {"snapshots/metadata.db", unix.S_IFREG}, {dsgDeletionJournal, unix.S_IFREG},
}

type dsgDeletionLayout struct {
	TargetID, PeerID, PeerKey string
	Identities                [len(dsgDeletionPaths)]dsgDeletionIdentity
}

type dsgDeletionFixture struct {
	dir    *os.File
	root   string
	plan   dsgRecoveryPlan
	layout dsgDeletionLayout
	db     *bolt.DB
	calls  int
}

func dsgDeletionHash(data any) string {
	encoded, _ := json.Marshal(data) // structs, strings and arrays only
	return dsgRecoveryHash(encoded)
}

func (s *dsgDeletionFixture) payload(domain, role string) string {
	return "native deletion fixture " + s.plan.Request.Generation + "/" + domain + "/" + role
}

func (s *dsgDeletionFixture) blob(role string) ocispec.Descriptor {
	return dsgBackingDescriptor(s.payload("blob", role))
}

// Open only the exact private layout created in this test. The root used by the
// common fence and by native APIs must still name the inode inherited by fd.
func (s *dsgDeletionFixture) identities() (out [len(dsgDeletionPaths)]dsgDeletionIdentity, err error) {
	anchor, err := dsgAllocationStatAt(int(s.dir.Fd()), "")
	named, namedErr := dsgAllocationStatAt(unix.AT_FDCWD, s.root)
	var fs unix.Statfs_t
	if err != nil || namedErr != nil || anchor != named || filepath.Base(s.root) != dsgDeletionRoot ||
		anchor.UID != uint32(os.Geteuid()) || anchor.Mode != unix.S_IFDIR|0700 ||
		unix.Fstatfs(int(s.dir.Fd()), &fs) != nil || int64(fs.Type) != int64(unix.EXT4_SUPER_MAGIC) ||
		fs.Flags&unix.ST_RDONLY != 0 {
		return out, ErrDSGExactPreserve
	}
	for i, path := range dsgDeletionPaths {
		st := anchor
		if path.rel != "" {
			fd, openErr := unix.Openat2(int(s.dir.Fd()), path.rel, &unix.OpenHow{
				Flags:   unix.O_PATH | unix.O_CLOEXEC,
				Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
			})
			if openErr != nil {
				return out, ErrDSGExactPreserve
			}
			st, err = dsgAllocationStatAt(fd, "")
			unix.Close(fd)
		}
		if err != nil || !st.ownedBy(anchor) || st.Mode&unix.S_IFMT != path.mode ||
			(path.mode == unix.S_IFREG && (st.Mode != unix.S_IFREG|0600 || st.Links != 1)) {
			return out, ErrDSGExactPreserve
		}
		out[i] = dsgDeletionIdentity{st.MountID, st.Inode, st.DeviceMajor, st.DeviceMinor}
	}
	return out, nil
}

// Compare every name in a small, fixture-owned directory. The optional target
// can be present or gone. Any staging trash (including rm-ID) is ambiguous.
func (s *dsgDeletionFixture) names(rel string, required []string, optional string) (bool, error) {
	fd, err := unix.Openat2(int(s.dir.Fd()), rel, &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_DIRECTORY | unix.O_CLOEXEC | unix.O_NOATIME,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
	})
	if err != nil {
		return false, ErrDSGExactPreserve
	}
	dir := os.NewFile(uintptr(fd), "dsg-deletion-directory")
	defer dir.Close()
	names, err := dir.Readdirnames(len(required) + 2)
	if err != nil && !errors.Is(err, io.EOF) {
		return false, err
	}
	target := optional != "" && slices.Contains(names, optional)
	expected := slices.Clone(required)
	if target {
		expected = append(expected, optional)
	}
	slices.Sort(expected)
	slices.Sort(names)
	if !slices.Equal(names, expected) {
		return false, ErrDSGExactPreserve
	}
	return target, nil
}

func (s *dsgDeletionFixture) payloadHash(ctx context.Context, parts []string, expected string) (string, error) {
	allocation, err := dsgObserveAllocation(ctx, int(s.dir.Fd()), parts, nil, false, uint64(len(expected)))
	if err != nil || allocation.Entries != 1 || allocation.AllocatedBlocks512 == 0 {
		return "", ErrDSGExactPreserve
	}
	fd, err := unix.Openat2(int(s.dir.Fd()), filepath.Join(parts...), &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_NOATIME | unix.O_CLOEXEC | unix.O_NONBLOCK,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_XDEV | unix.RESOLVE_NO_SYMLINKS,
	})
	if err != nil {
		return "", ErrDSGExactPreserve
	}
	f := os.NewFile(uintptr(fd), "dsg-deletion-payload")
	defer f.Close()
	before, err := dsgAllocationStatAt(fd, "")
	if err != nil || before.Inode != allocation.RootInode || before.Size != uint64(len(expected)) ||
		before.Mode&unix.S_IFMT != unix.S_IFREG || before.Links != 1 {
		return "", ErrDSGExactPreserve
	}
	raw, err := io.ReadAll(io.LimitReader(f, int64(len(expected)+1)))
	if err != nil || !bytes.Equal(raw, []byte(expected)) || !dsgAllocationMatches(fd, "", before) {
		return "", ErrDSGExactPreserve
	}
	return dsgDeletionHash([]string{allocation.MetadataSHA256, dsgRecoveryHash(raw)}), nil
}

func (s *dsgDeletionFixture) snapshotPayload(ctx context.Context, id, role string) (string, error) {
	dir := []string{"snapshots", "snapshots", id}
	if _, err := s.names(filepath.Join(dir...), []string{"payload"}, ""); err != nil {
		return "", err
	}
	allocation, err := dsgObserveAllocation(ctx, int(s.dir.Fd()), dir, nil, true, 0)
	if err != nil || allocation.Entries != 2 || allocation.AllocatedBlocks512 == 0 {
		return "", ErrDSGExactPreserve
	}
	payload, err := s.payloadHash(ctx, append(dir, "payload"), s.payload("snapshot", role))
	return dsgDeletionHash([]string{allocation.MetadataSHA256, payload}), err
}

func (s *dsgDeletionFixture) snapshot(ctx context.Context) (target, peer string, err error) {
	layout := s.layout
	names, err := s.names("snapshots/snapshots", []string{layout.PeerID}, layout.TargetID)
	if err != nil {
		return "", "", err
	}
	peerFile, err := s.snapshotPayload(ctx, layout.PeerID, "peer")
	if err != nil {
		return "", "", err
	}
	db, err := bolt.Open(filepath.Join(s.root, "snapshots", "metadata.db"), 0600, &bolt.Options{ReadOnly: true, Timeout: time.Second})
	if err != nil {
		return "", "", err
	}
	defer db.Close()
	err = db.View(func(tx *bolt.Tx) error {
		v1 := tx.Bucket([]byte("v1"))
		if v1 == nil {
			return ErrDSGExactPreserve
		}
		parents, table := v1.Bucket([]byte("parents")), v1.Bucket([]byte("snapshots"))
		if (parents != nil && parents.Stats().KeyN != 0) || table == nil {
			return ErrDSGExactPreserve
		}
		targetKey := s.plan.Request.RecordID
		snapshot := func(b *bolt.Bucket, id string) bool {
			if b == nil || len(b.Get([]byte("parent"))) != 0 ||
				!bytes.Equal(b.Get([]byte("kind")), []byte{byte(snapshots.KindCommitted)}) {
				return false
			}
			raw := b.Get([]byte("id"))
			value, size := binary.Uvarint(raw)
			labels, err := boltutil.ReadLabels(b)
			return size > 0 && size == len(raw) && strconv.FormatUint(value, 10) == id &&
				err == nil && len(labels) == 1 && labels[dsgDeletionFixtureLabel] == s.plan.Request.Generation
		}
		if err := table.ForEach(func(k, v []byte) error {
			switch key := string(k); {
			case v == nil && key == layout.PeerKey && snapshot(table.Bucket(k), layout.PeerID):
				return nil
			case v == nil && names && key == targetKey && snapshot(table.Bucket(k), layout.TargetID):
				return nil
			default:
				return ErrDSGExactPreserve
			}
		}); err != nil || table.Bucket([]byte(layout.PeerKey)) == nil ||
			(table.Bucket([]byte(targetKey)) != nil) != names {
			return ErrDSGExactPreserve
		}
		peerDB, err := dsgTrialTree(tx, func(path []string) bool {
			return slices.Equal(path, []string{"v1", "snapshots", targetKey})
		})
		if err != nil {
			return err
		}
		peer = dsgDeletionHash([]string{peerDB, peerFile})
		if !names {
			return nil
		}
		metadata, err := dsgTrialTree(tx, func([]string) bool { return false })
		if err != nil {
			return err
		}
		file, err := s.snapshotPayload(ctx, layout.TargetID, "target")
		target = dsgDeletionHash([]string{metadata, file})
		return err
	})
	return target, peer, err
}

func (s *dsgDeletionFixture) content(ctx context.Context) (target, peer string, err error) {
	if _, err := s.names("content", []string{"blobs", "ingest"}, ""); err != nil {
		return "", "", err
	}
	if _, err := s.names("content/ingest", nil, ""); err != nil {
		return "", "", err
	}
	if _, err := s.names("content/blobs", []string{"sha256"}, ""); err != nil {
		return "", "", err
	}
	targetBlob, peerBlob := s.blob("target"), s.blob("peer")
	if targetBlob.Digest.String() != s.plan.Objects[1].Key {
		return "", "", ErrDSGExactPreserve
	}
	present, err := s.names("content/blobs/sha256", []string{peerBlob.Digest.Encoded()}, targetBlob.Digest.Encoded())
	if err != nil {
		return "", "", err
	}
	peer, err = s.payloadHash(ctx, []string{"content", "blobs", "sha256", peerBlob.Digest.Encoded()}, s.payload("blob", "peer"))
	if err != nil || !present {
		return "", peer, err
	}
	target, err = s.payloadHash(ctx, []string{"content", "blobs", "sha256", targetBlob.Digest.Encoded()}, s.payload("blob", "target"))
	return target, peer, err
}
