//go:build linux && arm64

package dsgimage

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"syscall"
	"time"

	"golang.org/x/sys/unix"
)

const (
	modeMarker  = "dsg-dedicated-image-store"
	storeMarker = "docker-backend-store-id"
	maxFrame    = 4096
)

var (
	hashPattern  = regexp.MustCompile("^[a-f0-9]{64}$")
	imagePattern = regexp.MustCompile("^sha256:[a-f0-9]{64}$")
	privateLoop  = regexp.MustCompile("^/dev/loop[0-9]+$")
	errBoundary  = errors.New("dedicated image boundary unavailable")
)

type binding struct {
	Project    int64  `json:"project"`
	Owner      string `json:"owner"`
	Kind       string `json:"kind"`
	ID         string `json:"id"`
	Endpoint   string `json:"endpoint"`
	Generation string `json:"generation"`
	Snapshot   string `json:"snapshot"`
}

type intent struct {
	Operation string  `json:"operation"`
	Policy    string  `json:"policy"`
	Binding   binding `json:"binding"`
}

type request struct {
	Version   int     `json:"version"`
	Operation string  `json:"operation"`
	ID        string  `json:"image_id,omitempty"`
	Owner     string  `json:"owner,omitempty"`
	Intent    *intent `json:"intent,omitempty"`
}

type receipt struct {
	Intent     intent      `json:"intent"`
	Bytes      uint64      `json:"reclaimed_bytes"`
	Allocation *allocation `json:"allocation,omitempty"`
}

type response struct {
	Version   int      `json:"version"`
	Status    string   `json:"status"`
	StoreID   string   `json:"store_id"`
	Snapshot  string   `json:"snapshot,omitempty"`
	Estimated *uint64  `json:"estimated_exclusive_layer_bytes,omitempty"`
	Receipt   *receipt `json:"receipt,omitempty"`
}

type pinnedController struct {
	pid   int
	pidfd int
	info  os.FileInfo
}

func controller(pid int, expected string) (*pinnedController, error) {
	if pid <= 1 || !hashPattern.MatchString(expected) {
		return nil, errBoundary
	}
	fd, err := unix.PidfdOpen(pid, 0)
	if err != nil {
		return nil, errBoundary
	}
	file, err := os.Open(fmt.Sprintf("/proc/%d/exe", pid))
	if err != nil {
		unix.Close(fd)
		return nil, errBoundary
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&022 != 0 || !rootOwned(info) || info.Size() <= 0 || info.Size() > 128<<20 {
		unix.Close(fd)
		return nil, errBoundary
	}
	h := sha256.New()
	size, err := io.Copy(h, io.LimitReader(file, info.Size()+1))
	if err != nil || size != info.Size() || hex.EncodeToString(h.Sum(nil)) != expected {
		unix.Close(fd)
		return nil, errBoundary
	}
	return &pinnedController{pid: pid, pidfd: fd, info: info}, nil
}

func (p *pinnedController) accepts(conn *net.UnixConn) bool {
	raw, err := conn.SyscallConn()
	if err != nil {
		return false
	}
	var cred *unix.Ucred
	if raw.Control(func(fd uintptr) {
		cred, err = unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED)
	}) != nil || err != nil || cred == nil || cred.Uid != 0 || int(cred.Pid) != p.pid {
		return false
	}
	fds := []unix.PollFd{{Fd: int32(p.pidfd), Events: unix.POLLIN}}
	count, err := unix.Poll(fds, 0)
	info, fserr := os.Stat(fmt.Sprintf("/proc/%d/exe", p.pid))
	return err == nil && count == 0 && fserr == nil && os.SameFile(info, p.info)
}

func protectedPath(path string, directory bool, mode os.FileMode) bool {
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || path == "/" {
		return false
	}
	for part := path; part != "/"; part = filepath.Dir(part) {
		info, err := os.Lstat(part)
		if err != nil || !rootOwned(info) || info.Mode().Perm()&022 != 0 {
			return false
		}
		if part == path && !directory {
			if !info.Mode().IsRegular() || info.Mode().Perm() != mode {
				return false
			}
		} else if !info.IsDir() || (part == path && info.Mode().Perm() != mode) {
			return false
		}
	}
	return true
}

func rootOwned(info os.FileInfo) bool {
	return info.Sys().(*syscall.Stat_t).Uid == 0
}

func marker(path string, size int) ([]byte, error) {
	if !protectedPath(path, false, 0444) {
		return nil, errBoundary
	}
	fd, err := unix.Open(path, unix.O_NOFOLLOW|unix.O_CLOEXEC|unix.O_NONBLOCK|unix.O_RDONLY, 0)
	if err != nil {
		return nil, errBoundary
	}
	f := os.NewFile(uintptr(fd), path)
	defer f.Close()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm() != 0444 || !rootOwned(info) || info.Sys().(*syscall.Stat_t).Nlink != 1 || info.Size() != int64(size) {
		return nil, errBoundary
	}
	return io.ReadAll(io.LimitReader(f, int64(size+1)))
}

func privateMount(root string, device uint64) bool {
	table, err := os.Open("/proc/self/mountinfo")
	if err != nil {
		return false
	}
	defer table.Close()
	if unix.Major(device) != 7 || strings.ContainsAny(root, " \t\n\\") {
		return false
	}
	name := fmt.Sprintf("%d:%d", unix.Major(device), unix.Minor(device))
	const maxMountInfo = 1 << 20
	raw, err := io.ReadAll(io.LimitReader(table, maxMountInfo+1))
	if err != nil || len(raw) > maxMountInfo {
		return false
	}
	scan := bufio.NewScanner(bytes.NewReader(raw))
	count := 0
	for scan.Scan() {
		left, right, ok := strings.Cut(scan.Text(), " - ")
		fields, fs := strings.Fields(left), strings.Fields(right)
		if !ok || len(fields) < 6 || len(fs) != 3 {
			return false
		}
		if fields[2] == name || fields[4] == root || strings.HasPrefix(fields[4], root+"/") {
			if fields[2] != name || fields[3] != "/" || fields[4] != root || len(fields) != 6 || fs[0] != "ext4" || !privateLoop.MatchString(fs[1]) {
				return false
			}
			opts := "," + fields[5] + ","
			if !strings.Contains(opts, ",rw,") || !strings.Contains(opts, ",nodev,") || !strings.Contains(opts, ",nosuid,") {
				return false
			}
			count++
		}
	}
	if scan.Err() != nil || count != 1 {
		return false
	}
	host, err := os.Open("/proc/1/mountinfo")
	if err != nil {
		return false
	}
	defer host.Close()
	hostScan := bufio.NewScanner(host)
	for hostScan.Scan() {
		fields := strings.Fields(hostScan.Text())
		if len(fields) < 6 || fields[2] == name {
			return false
		}
	}
	return hostScan.Err() == nil
}

func namespacesPrivate() bool {
	for _, kind := range []string{"mnt", "net"} {
		current, err := os.Readlink("/proc/self/ns/" + kind)
		host, hostErr := os.Readlink("/proc/1/ns/" + kind)
		if err != nil || hostErr != nil || current == host {
			return false
		}
	}
	return true
}

func noOtherWriters() bool {
	namespace, err := os.Readlink("/proc/self/ns/mnt")
	if err != nil {
		return false
	}
	proc, err := os.Open("/proc")
	if err != nil {
		return false
	}
	defer proc.Close()
	names, err := proc.Readdirnames(16385)
	if err != nil && err != io.EOF || len(names) > 16384 {
		return false
	}
	for _, name := range names {
		pid, err := strconv.Atoi(name)
		if err != nil || pid == os.Getpid() {
			continue
		}
		dir := filepath.Join("/proc", name)
		other, err := os.Readlink(filepath.Join(dir, "ns/mnt"))
		if errors.Is(err, os.ErrNotExist) || (err == nil && other != namespace) {
			continue
		}
		if err != nil {
			return false
		}
		command, err := os.ReadFile(filepath.Join(dir, "comm"))
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return false
		}
		for _, writer := range []string{"dockerd", "containerd", "buildkitd", "runc"} {
			if bytes.HasPrefix(command, []byte(writer)) {
				return false
			}
		}
	}
	return true
}

type service struct {
	backend   *imageBackend
	storeID   string
	namespace string
	root      string
	mount     string
	control   *pinnedController
	dir       *os.File
	ledger    *os.File
	rootInfo  os.FileInfo
}

func (s *service) current() bool {
	got, err := s.dir.Stat()
	live, liveErr := os.Lstat(s.mount)
	if err != nil || liveErr != nil || !os.SameFile(got, live) || !privateMount(s.mount, got.Sys().(*syscall.Stat_t).Dev) {
		return false
	}
	root, rootErr := os.Lstat(s.root)
	ledger, ledgerErr := s.ledger.Stat()
	ledgerPath, ledgerPathErr := os.Lstat(filepath.Join(s.mount, "dsg-image-receipts"))
	if rootErr != nil || ledgerErr != nil || ledgerPathErr != nil || !os.SameFile(root, s.rootInfo) || !os.SameFile(ledger, ledgerPath) ||
		!rootOwned(got) || !rootOwned(root) || !rootOwned(ledger) ||
		got.Mode().Perm() != 0700 || root.Mode().Perm() != 0700 || ledger.Mode().Perm() != 0700 {
		return false
	}
	identity, err := marker(filepath.Join(s.root, storeMarker), 65)
	return err == nil && string(identity) == s.storeID+"\n" && noOtherWriters()
}

// Serve never starts daemon.NewDaemon, a stock Docker listener, containerd or BuildKit.
func Serve(ctx context.Context, root, socket string, pid int, executable string) error {
	if os.Geteuid() != 0 || filepath.Base(root) != "docker-data" || !protectedPath(root, true, 0700) ||
		!filepath.IsAbs(socket) || filepath.Clean(socket) != socket || !protectedPath(filepath.Dir(socket), true, 0700) ||
		!namespacesPrivate() {
		return errBoundary
	}
	mount := filepath.Dir(root)
	if !protectedPath(mount, true, 0700) {
		return errBoundary
	}
	dir, err := os.Open(mount)
	if err != nil {
		return errBoundary
	}
	defer dir.Close()
	info, err := dir.Stat()
	if err != nil || !privateMount(mount, info.Sys().(*syscall.Stat_t).Dev) ||
		unix.Flock(int(dir.Fd()), unix.LOCK_EX|unix.LOCK_NB) != nil {
		return errBoundary
	}
	enrolled, err := marker(filepath.Join(mount, modeMarker), len("classic-vfs-archive-v1\n"))
	if err != nil || string(enrolled) != "classic-vfs-archive-v1\n" {
		return errBoundary
	}
	source, err := marker(filepath.Join(root, storeMarker), 65)
	if err != nil {
		return errBoundary
	}
	storeID := strings.TrimSuffix(string(source), "\n")
	if !hashPattern.MatchString(storeID) || len(source) != 65 {
		return errBoundary
	}
	pinned, err := controller(pid, executable)
	if err != nil {
		return err
	}
	defer unix.Close(pinned.pidfd)
	rootInfo, err := os.Lstat(root)
	if err != nil || rootInfo.Sys().(*syscall.Stat_t).Dev != info.Sys().(*syscall.Stat_t).Dev {
		return errBoundary
	}
	if !noOtherWriters() {
		return errBoundary
	}
	backend, err := openImageStore(root)
	if err != nil {
		return errBoundary
	}
	ledgerPath := filepath.Join(mount, "dsg-image-receipts")
	if err = os.Mkdir(ledgerPath, 0700); err != nil && !errors.Is(err, os.ErrExist) {
		return errBoundary
	}
	if !protectedPath(ledgerPath, true, 0700) {
		return errBoundary
	}
	ledger, err := os.Open(ledgerPath)
	if err != nil {
		return errBoundary
	}
	defer ledger.Close()
	ledgerInfo, err := ledger.Stat()
	if err != nil || ledgerInfo.Sys().(*syscall.Stat_t).Dev != info.Sys().(*syscall.Stat_t).Dev || dir.Sync() != nil {
		return errBoundary
	}
	service := service{backend: backend, storeID: storeID,
		namespace: fmt.Sprintf("%x", sha256.Sum256([]byte("dsg-docker-backend-store-v1\x00"+storeID))),
		root:      root, mount: mount, control: pinned, dir: dir, ledger: ledger, rootInfo: rootInfo}
	if !service.current() {
		return errBoundary
	}
	oldMask := unix.Umask(0177)
	listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: socket, Net: "unix"})
	unix.Umask(oldMask)
	if err != nil {
		return errBoundary
	}
	defer listener.Close()
	for ctx.Err() == nil {
		if listener.SetDeadline(time.Now().Add(time.Second)) != nil {
			return errBoundary
		}
		conn, err := listener.AcceptUnix()
		if err != nil {
			if n, ok := err.(net.Error); ok && n.Timeout() {
				continue
			}
			return errBoundary
		}
		service.exchange(conn)
	}
	return nil
}

func readRequest(input *bufio.Reader) (request, error) {
	var msg request
	line, err := input.ReadSlice('\n')
	if err != nil || len(line) > maxFrame || len(line) <= 1 {
		return msg, errBoundary
	}
	if err = decode(line, &msg); err != nil || msg.Version != 1 {
		return request{}, errBoundary
	}
	return msg, nil
}

func decode(data []byte, out any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if dec.Decode(out) != nil || dec.Decode(new(any)) != io.EOF {
		return errBoundary
	}
	var compact bytes.Buffer
	canonical, err := json.Marshal(out)
	if err != nil || json.Compact(&compact, data) != nil || !bytes.Equal(compact.Bytes(), canonical) {
		return errBoundary
	}
	return nil
}

func (s *service) send(c *net.UnixConn, status string, r *receipt) {
	_ = json.NewEncoder(c).Encode(response{Version: 1, Status: status, StoreID: s.storeID, Receipt: r})
}

func (s *service) validIntent(id string, value *intent) bool {
	if value == nil || !hashPattern.MatchString(value.Operation) || !hashPattern.MatchString(value.Policy) {
		return false
	}
	b := value.Binding
	return b.Project > 0 && b.Kind == "image" && b.ID == id && b.Endpoint == s.namespace &&
		hashPattern.MatchString(b.Owner) && hashPattern.MatchString(b.Generation) && hashPattern.MatchString(b.Snapshot)
}

// The serial listener owns the only writer in this opened classic store. It cannot accept
// a second request between ready and commit. Deadlines and socket closure end a prepare.
func (s *service) exchange(conn *net.UnixConn) {
	defer conn.Close()
	if !s.control.accepts(conn) || !s.current() {
		return
	}
	deadline := time.Now().Add(8 * time.Second)
	if conn.SetDeadline(deadline) != nil {
		return
	}
	input := bufio.NewReaderSize(conn, maxFrame)
	msg, err := readRequest(input)
	if err != nil {
		return
	}
	if msg.Operation == "probe" && msg.ID == "" && msg.Intent == nil && msg.Owner == "" {
		s.send(conn, "ready", nil)
		return
	}
	if !imagePattern.MatchString(msg.ID) {
		s.send(conn, "preserved", nil)
		return
	}
	owner := msg.Owner
	switch {
	case msg.Operation == "observe" && msg.Intent == nil && hashPattern.MatchString(owner):
	case msg.Owner == "" && (msg.Operation == "prepare" || msg.Operation == "receipt") && s.validIntent(msg.ID, msg.Intent):
		owner = msg.Intent.Binding.Owner
	default:
		s.send(conn, "preserved", nil)
		return
	}
	if msg.Operation == "receipt" {
		r := s.receipt(*msg.Intent)
		if r == nil {
			s.send(conn, "preserved", nil)
		} else {
			s.send(conn, "settled", r)
		}
		return
	}
	if s.hasAttempt(msg.ID) {
		s.send(conn, "preserved", nil)
		return
	}
	snapshot, estimated, err := s.backend.observe(msg.ID, owner)
	if err != nil || !hashPattern.MatchString(snapshot) ||
		(msg.Intent != nil && snapshot != msg.Intent.Binding.Snapshot) {
		s.send(conn, "preserved", nil)
		return
	}
	if err = json.NewEncoder(conn).Encode(response{Version: 1, Status: "ready", StoreID: s.storeID, Snapshot: snapshot, Estimated: &estimated}); err != nil {
		return
	}
	if msg.Operation == "observe" {
		return
	}
	next, err := readRequest(input)
	if err != nil || next.Operation != "commit" || next.ID != msg.ID || next.Owner != "" ||
		next.Intent == nil || *next.Intent != *msg.Intent || !s.control.accepts(conn) || !s.current() {
		return
	}
	// Once the marker reaches disk, any interruption conservatively blocks all further
	// attempts for this exact image. A missing receipt never authorizes another delete.
	baseline, measurable := sampleAllocation(s.dir)
	if err = s.publish(attemptFile(msg.ID), *msg.Intent); err != nil {
		return
	}
	receiptName := msg.Intent.Operation + ".receipt"
	var slot *os.File
	if measurable {
		slot, err = s.reserveReceipt(receiptName)
		if err != nil {
			return
		}
		defer slot.Close()
	}
	if err = s.backend.remove(msg.ID, snapshot, owner, func() bool { return time.Now().Before(deadline) && s.control.accepts(conn) && s.current() }); err != nil {
		return
	}
	if unix.Syncfs(int(s.dir.Fd())) != nil || !s.current() {
		return
	}
	value := receipt{Intent: *msg.Intent}
	if slot != nil {
		err = s.settleReceipt(slot, value, baseline, estimated)
	} else {
		err = s.publishReceipt(receiptName, value)
	}
	if err != nil {
		return
	}
	s.send(conn, "committed", nil)
}

func attemptFile(id string) string { return strings.TrimPrefix(id, "sha256:") + ".attempt" }

func (s *service) hasAttempt(id string) bool {
	var info unix.Stat_t
	return !errors.Is(unix.Fstatat(int(s.ledger.Fd()), attemptFile(id), &info, unix.AT_SYMLINK_NOFOLLOW), unix.ENOENT)
}

func (s *service) publish(name string, value any) error {
	fd, err := unix.Openat(int(s.ledger.Fd()), name, unix.O_WRONLY|unix.O_CREAT|unix.O_EXCL|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return errBoundary
	}
	f := os.NewFile(uintptr(fd), name)
	writeErr := json.NewEncoder(f).Encode(value)
	syncErr := f.Sync()
	closeErr := f.Close()
	if writeErr != nil || syncErr != nil || closeErr != nil || s.ledger.Sync() != nil {
		return errBoundary
	}
	return nil
}

func (s *service) publishReceipt(name string, value receipt) error {
	temporary := name + ".pending"
	if s.publish(temporary, value) != nil {
		return errBoundary
	}
	dir := int(s.ledger.Fd())
	if unix.Renameat2(dir, temporary, dir, name, unix.RENAME_NOREPLACE) != nil {
		return errBoundary
	}
	return s.ledger.Sync()
}

func (s *service) ledgerContents(name string, durable bool) []byte {
	fd, err := unix.Openat(int(s.ledger.Fd()), name, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	if err != nil {
		return nil
	}
	f := os.NewFile(uintptr(fd), name)
	defer f.Close()
	stat, err := f.Stat()
	if err != nil || !stat.Mode().IsRegular() || !rootOwned(stat) || stat.Mode().Perm() != 0600 || stat.Sys().(*syscall.Stat_t).Nlink != 1 ||
		stat.Size() <= 0 || stat.Size() > maxFrame || (durable && f.Sync() != nil) {
		return nil
	}
	content, err := io.ReadAll(io.LimitReader(f, maxFrame+1))
	if err != nil || int64(len(content)) != stat.Size() {
		return nil
	}
	return content
}

func (s *service) receipt(want intent) *receipt {
	var before intent
	if s.ledger.Sync() != nil || decode(s.ledgerContents(attemptFile(want.Binding.ID), false), &before) != nil || before != want {
		return nil
	}
	got := decodeReceipt(s.ledgerContents(want.Operation+".receipt", true))
	if got == nil || got.Intent != want {
		return nil
	}
	return got
}
