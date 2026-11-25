//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
	"time"

	bolt "go.etcd.io/bbolt"
	"golang.org/x/sys/unix"
)

type dsgSameMountAuthority struct {
	db             *bolt.DB
	device         uint64
	inode          uint64
	path           string
	mu             sync.Mutex
	witness        *dsgLedgerWitness
	claimFreshness func(context.Context, dsgSameMountGrant, string) error
}

const dsgLedgerLimit = 16 << 20

type dsgWitnessIdentity struct {
	Device uint64 `json:"device"`
	Inode  uint64 `json:"inode"`
}

type dsgLedgerWitness struct {
	path         string
	identity     dsgWitnessIdentity
	workerDevice uint64
}

func (w dsgLedgerWitness) open(flags int, ledgerDevice uint64) (*os.File, error) {
	var base unix.Stat_t
	if unix.Lstat(filepath.Dir(filepath.Dir(w.path)), &base) != nil || int(base.Uid) != os.Geteuid() ||
		base.Mode != unix.S_IFDIR|0700 || uint64(base.Dev) != w.identity.Device || w.identity.Device == ledgerDevice || w.identity.Device == w.workerDevice {
		return nil, ErrDSGExactPreserve
	}
	f, st, err := dsgLedgerFile(w.path, flags)
	if err != nil {
		return nil, err
	}
	var fs unix.Statfs_t
	if unix.Fstatfs(int(f.Fd()), &fs) != nil || fs.Type == unix.TMPFS_MAGIC || uint32(fs.Type) == unix.RAMFS_MAGIC ||
		w.identity.Device != uint64(st.Dev) || w.identity.Inode != st.Ino || st.Size != 2*sha256.Size {
		f.Close()
		return nil, ErrDSGExactPreserve
	}
	return f, nil
}

func dsgWitnessEqual(f *os.File, expected []byte) bool {
	var data [2 * sha256.Size]byte
	_, err := f.ReadAt(data[:], 0)
	return err == nil && bytes.Equal(data[:], expected)
}

func (w dsgLedgerWitness) matches(expected []byte, ledgerDevice uint64) bool {
	f, err := w.open(unix.O_RDONLY, ledgerDevice)
	if err != nil {
		return false
	}
	defer f.Close()
	return dsgWitnessEqual(f, expected)
}

// Update only the enrolled inode, and only if it still holds the digest
// observed before the Bolt transaction. An interrupted update stays closed.
func (w dsgLedgerWitness) advance(previous, next []byte, ledgerDevice uint64) error {
	f, err := w.open(unix.O_RDWR, ledgerDevice)
	if err != nil {
		return err
	}
	defer f.Close()
	if !dsgWitnessEqual(f, previous) {
		return ErrDSGExactPreserve
	}
	if _, err := f.WriteAt(next, 0); err != nil || f.Sync() != nil {
		return ErrDSGExactPreserve
	}
	return nil
}

// Open an existing supervised file, or create it exclusively at first
// enrollment. Never give bbolt a missing or empty file during recovery.
func dsgLedgerFile(path string, flags int) (*os.File, unix.Stat_t, error) {
	var st unix.Stat_t
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || !dsgProtectedParent(path) {
		return nil, st, ErrDSGExactPreserve
	}
	dir, err := unix.Open(filepath.Dir(path), unix.O_DIRECTORY|unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, st, ErrDSGExactPreserve
	}
	defer unix.Close(dir)
	var parent unix.Stat_t
	if unix.Fstat(dir, &parent) != nil || int(parent.Uid) != os.Geteuid() || parent.Mode&07777 != 0700 {
		return nil, st, ErrDSGExactPreserve
	}
	fd, err := unix.Openat(dir, filepath.Base(path), flags|unix.O_CLOEXEC|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0600)
	if err != nil {
		return nil, st, ErrDSGExactPreserve
	}
	if unix.Fstat(fd, &st) != nil || st.Mode != unix.S_IFREG|0600 || int(st.Uid) != os.Geteuid() ||
		st.Dev == 0 || st.Dev != parent.Dev || st.Nlink != 1 {
		unix.Close(fd)
		return nil, st, ErrDSGExactPreserve
	}
	return os.NewFile(uintptr(fd), path), st, nil
}

// Bind the bounded Bolt contents to the original file identity. Read at a
// fixed offset so the check never changes the descriptor position for bbolt.
func dsgLedgerDigest(file *os.File, st unix.Stat_t) ([]byte, error) {
	if st.Size <= 0 || st.Size > dsgLedgerLimit {
		return nil, ErrDSGExactPreserve
	}
	h := sha256.New()
	fmt.Fprintf(h, "dsg-same-mount-ledger-v1\x00%s\x00%d/%d/%d\x00", file.Name(), st.Dev, st.Ino, st.Size)
	n, err := io.Copy(h, io.NewSectionReader(file, 0, st.Size))
	var after unix.Stat_t
	if err != nil || n != st.Size || unix.Fstat(int(file.Fd()), &after) != nil ||
		after.Size != st.Size || after.Mtim != st.Mtim || after.Ctim != st.Ctim {
		return nil, ErrDSGExactPreserve
	}
	return []byte(hex.EncodeToString(h.Sum(nil))), nil
}

func (a *dsgSameMountAuthority) matchedDigest(file *os.File, st unix.Stat_t) []byte {
	witness, ws, err := dsgLedgerFile(a.path+".checkpoint", unix.O_RDONLY)
	if err != nil {
		return nil
	}
	defer witness.Close()
	if ws.Size != 2*sha256.Size || ws.Dev != st.Dev || ws.Ino == st.Ino {
		return nil
	}
	expected := make([]byte, ws.Size)
	_, err = io.ReadFull(witness, expected)
	actual, digestErr := dsgLedgerDigest(file, st)
	if err != nil || digestErr != nil || !bytes.Equal(expected, actual) || (a.witness != nil && !a.witness.matches(actual, uint64(st.Dev))) {
		return nil
	}
	return actual
}

func dsgCreateSameMountAuthority(path string) (*dsgSameMountAuthority, error) {
	return dsgAccessSameMountAuthority(path, true, nil)
}

func dsgOpenSameMountAuthority(path string) (*dsgSameMountAuthority, error) {
	return dsgAccessSameMountAuthority(path, false, nil)
}

func dsgAccessSameMountAuthority(path string, create bool, witness *dsgLedgerWitness) (*dsgSameMountAuthority, error) {
	a := &dsgSameMountAuthority{path: path, witness: witness}
	flags := unix.O_RDWR
	if create {
		// Any existing checkpoint identifies prior enrollment even when the
		// corresponding database is missing. Leave both paths untouched.
		if _, err := os.Lstat(path + ".checkpoint"); !errors.Is(err, os.ErrNotExist) {
			return nil, ErrDSGExactPreserve
		}
		flags |= unix.O_CREAT | unix.O_EXCL
	}
	db, err := bolt.Open(path, 0600, &bolt.Options{Timeout: time.Second, OpenFile: func(_ string, _ int, _ os.FileMode) (*os.File, error) {
		f, st, err := dsgLedgerFile(path, flags)
		if err != nil {
			return nil, err
		}
		if !create && a.matchedDigest(f, st) == nil {
			f.Close()
			return nil, ErrDSGExactPreserve
		}
		a.device, a.inode = uint64(st.Dev), st.Ino
		return f, nil
	}})
	if err != nil {
		return nil, ErrDSGExactPreserve
	}
	a.db = db
	if create {
		err = a.checkpoint(true, nil)
	} else if !a.verified() {
		err = ErrDSGExactPreserve
	}
	if err != nil {
		db.Close()
		return nil, ErrDSGExactPreserve
	}
	return a, nil
}

func (a *dsgSameMountAuthority) verified() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.currentDigest() != nil
}

func (a *dsgSameMountAuthority) currentDigest() []byte {
	f, st, err := dsgLedgerFile(a.path, unix.O_RDONLY)
	if err != nil {
		return nil
	}
	defer f.Close()
	if uint64(st.Dev) != a.device || st.Ino != a.inode {
		return nil
	}
	return a.matchedDigest(f, st)
}

// Each durable phase precedes its checkpoint. An interruption between syncs
// leaves an untrusted ledger and stops mutation. Keep the last checkpoint at
// its separate path so restoring old grant bytes cannot restore authority.
func (a *dsgSameMountAuthority) update(fn func(*bolt.Tx) error) error {
	return a.updateIfCurrent(nil, fn)
}

func (a *dsgSameMountAuthority) updateIfCurrent(expected []byte, fn func(*bolt.Tx) error) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	previous := a.currentDigest()
	if previous == nil || (expected != nil && !bytes.Equal(previous, expected)) {
		return ErrDSGExactPreserve
	}
	if err := a.db.Update(fn); err != nil {
		return err
	}
	return a.checkpoint(false, previous)
}

func (a *dsgSameMountAuthority) checkpoint(create bool, previous []byte) error {
	f, st, err := dsgLedgerFile(a.path, unix.O_RDONLY)
	if err != nil {
		return err
	}
	if uint64(st.Dev) != a.device || st.Ino != a.inode {
		f.Close()
		return ErrDSGExactPreserve
	}
	digest, err := dsgLedgerDigest(f, st)
	f.Close()
	if err != nil {
		return err
	}
	flags := unix.O_WRONLY
	if create {
		flags |= unix.O_CREAT | unix.O_EXCL
	}
	witness, ws, err := dsgLedgerFile(a.path+".checkpoint", flags)
	if err != nil {
		return err
	}
	defer witness.Close()
	if (!create && ws.Size != int64(len(digest))) || ws.Ino == st.Ino || ws.Dev != st.Dev {
		return ErrDSGExactPreserve
	}
	if _, err := witness.WriteAt(digest, 0); err != nil || witness.Sync() != nil {
		return ErrDSGExactPreserve
	}
	if create {
		dir, err := os.Open(filepath.Dir(a.path))
		if err != nil {
			return ErrDSGExactPreserve
		}
		defer dir.Close()
		if dir.Sync() != nil {
			return ErrDSGExactPreserve
		}
	}
	if a.witness != nil {
		if err := a.witness.advance(previous, digest, a.device); err != nil || !bytes.Equal(a.currentDigest(), digest) {
			return ErrDSGExactPreserve
		}
	}
	return nil
}
