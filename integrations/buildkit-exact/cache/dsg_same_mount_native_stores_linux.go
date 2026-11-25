//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"

	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

const dsgDeletionRoot = "fresh-native-deletion"
const dsgDeletionJournal = "deletion-qualification.db"
const dsgManifestlessFile = "dsg-owned-payload"

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
	dir               *os.File
	root              string
	plan              dsgRecoveryPlan
	layout            dsgDeletionLayout
	db                *bolt.DB
	calls             int
	readOnly          bool
	payloadGeneration string
}

func dsgDeletionHash(data any) string {
	encoded, _ := json.Marshal(data) // structs, strings and arrays only
	return dsgRecoveryHash(encoded)
}

func (s *dsgDeletionFixture) payload(domain, role string) string {
	generation := s.payloadGeneration
	if generation == "" {
		generation = s.plan.Request.Generation
	}
	return "native deletion fixture " + generation + "/" + domain + "/" + role
}

// Open only the exact layout created by the isolated worker producer. The root used by the
// common fence and by native APIs must still name the inode inherited by fd.
func (s *dsgDeletionFixture) identities() (out [len(dsgDeletionPaths)]dsgDeletionIdentity, err error) {
	anchor, err := dsgAllocationStatAt(int(s.dir.Fd()), "")
	named, namedErr := dsgAllocationStatAt(unix.AT_FDCWD, s.root)
	var fs unix.Statfs_t
	if err != nil || namedErr != nil || anchor != named || filepath.Base(s.root) != dsgDeletionRoot ||
		anchor.UID != uint32(os.Geteuid()) || anchor.Mode != unix.S_IFDIR|0700 ||
		unix.Fstatfs(int(s.dir.Fd()), &fs) != nil || int64(fs.Type) != int64(unix.EXT4_SUPER_MAGIC) ||
		(fs.Flags&unix.ST_RDONLY != 0) != s.readOnly {
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

const dsgDeletionCalls = "_dsg_fixture_native_delete_calls_v1"

func (s *dsgDeletionFixture) callMarker(p dsgRecoveryPlan, stage int) (recorded bool, err error) {
	key := []byte(p.Request.Intent + "/" + p.stages()[stage])
	err = s.db.View(func(tx *bolt.Tx) error {
		b := tx.Bucket([]byte(dsgDeletionCalls))
		if b == nil {
			return nil
		}
		raw := b.Get(key)
		if b.Bucket(key) != nil || (raw != nil && string(raw) != p.proof(stage)) {
			return ErrDSGExactPreserve
		}
		recorded = raw != nil
		return nil
	})
	return recorded, err
}

func (s *dsgDeletionFixture) recordCall(ctx context.Context, p dsgRecoveryPlan, stage int) error {
	return s.db.Update(func(tx *bolt.Tx) error {
		state, err := dsgRecoveryLoad(tx, p)
		if err != nil || !state.Armed || state.Confirmed != stage || !dsgRecoveryAuthorized(ctx, p) {
			return ErrDSGExactPreserve
		}
		b, err := tx.CreateBucketIfNotExists([]byte(dsgDeletionCalls))
		if err != nil {
			return err
		}
		key := []byte(p.Request.Intent + "/" + p.stages()[stage])
		if b.Get(key) != nil || b.Bucket(key) != nil {
			return ErrDSGExactPreserve
		}
		return b.Put(key, []byte(p.proof(stage)))
	})
}

// Hash the full bounded native bucket tree, excluding only specified leaves
// (solver) or the single new protective lease (containerd). Length-framed
// components distinguish a key containing '/' from multiple path components.
func dsgTrialTree(tx *bolt.Tx, skip func([]string) bool) (string, error) {
	h := sha256.New()
	// bbolt keys may contain arbitrary bytes, including invalid UTF-8 in the
	// native parent index. JSON strings would normalize such keys to U+FFFD.
	writePath := func(kind byte, path []string) {
		fmt.Fprintf(h, "%c%d:", kind, len(path))
		for _, part := range path {
			fmt.Fprintf(h, "%d:", len(part))
			h.Write([]byte(part))
		}
	}
	remaining := 1024
	var visit func([]string, *bolt.Bucket) error
	visit = func(path []string, b *bolt.Bucket) error {
		if len(path) > 12 || remaining <= 0 {
			return ErrDSGExactPreserve
		}
		remaining--
		writePath('b', path)
		fmt.Fprintf(h, "s%d;", b.Sequence())
		return b.ForEach(func(k, v []byte) error {
			if len(k) > 512 || len(v) > 8192 || remaining <= 0 {
				return ErrDSGExactPreserve
			}
			child := append(slices.Clone(path), string(k))
			if skip(child) {
				return nil
			}
			if v == nil {
				return visit(child, b.Bucket(k))
			}
			remaining--
			writePath('v', child)
			fmt.Fprintf(h, "%d:", len(v))
			h.Write(v)
			return nil
		})
	}
	err := tx.ForEach(func(k []byte, b *bolt.Bucket) error {
		path := []string{string(k)}
		if skip(path) {
			return nil
		}
		return visit(path, b)
	})
	return fmt.Sprintf("%x", h.Sum(nil)), err
}
