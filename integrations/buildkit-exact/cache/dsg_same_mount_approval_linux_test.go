//go:build linux

package cache

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"golang.org/x/sys/unix"
)

// Only this isolated guest qualification command can perform the actual
// transition. The kernel follows the pinned mountpoint descriptor while the
// original enrolled child directory remains exclusively locked.
func dsgCustodyRemount(fd int, readOnly bool) error {
	parent, err := unix.Openat(fd, "..", unix.O_PATH|unix.O_NOFOLLOW|unix.O_DIRECTORY|unix.O_CLOEXEC, 0)
	if err != nil {
		return ErrDSGExactPreserve
	}
	defer unix.Close(parent)
	pst, err := dsgAllocationStatAt(parent, "")
	if err != nil {
		return err
	}
	child, err := dsgAllocationStatAt(fd, "")
	if err != nil || pst.MountID != child.MountID {
		return ErrDSGExactPreserve
	}
	mounts, err := dsgMounts("/proc/self/mountinfo")
	if err != nil {
		return err
	}
	for _, m := range mounts {
		if m.id != strconv.FormatUint(pst.MountID, 10) {
			continue
		}
		flags := uintptr(unix.MS_REMOUNT)
		if readOnly {
			flags |= unix.MS_RDONLY
		}
		for _, opt := range strings.Split(m.options, ",") {
			switch opt {
			case "rw", "ro":
			case "nodev":
				flags |= unix.MS_NODEV
			case "noexec":
				flags |= unix.MS_NOEXEC
			case "nosuid":
				flags |= unix.MS_NOSUID
			case "nosymfollow":
				flags |= unix.MS_NOSYMFOLLOW
			case "relatime":
				flags |= unix.MS_RELATIME
			case "strictatime":
				flags |= unix.MS_STRICTATIME
			case "nodiratime":
				flags |= unix.MS_NODIRATIME
			case "noatime":
				flags |= unix.MS_NOATIME
			default:
				return ErrDSGExactPreserve
			}
		}
		return unix.Mount("", "/proc/self/fd/"+strconv.Itoa(parent), "", flags, "")
	}
	return ErrDSGExactPreserve
}

func dsgCustodyAuthorityFile(t *testing.T, path string, device uint64) []byte {
	t.Helper()
	parent := filepath.Dir(path)
	require.True(t, filepath.IsAbs(path) && filepath.Clean(path) == path && dsgProtectedParent(path))
	var pst unix.Stat_t
	require.NoError(t, unix.Lstat(parent, &pst))
	require.True(t, pst.Uid == 0 && pst.Mode == unix.S_IFDIR|0700 && uint64(pst.Dev) != device)
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NONBLOCK|unix.O_NOFOLLOW, 0)
	require.NoError(t, err)
	file := os.NewFile(uintptr(fd), "dsg-authority-input")
	defer file.Close()
	var st unix.Stat_t
	require.NoError(t, unix.Fstat(fd, &st))
	require.True(t, st.Uid == 0 && st.Mode == unix.S_IFREG|0600 && st.Nlink == 1 && uint64(st.Dev) == uint64(pst.Dev) && st.Size > 0 && st.Size <= 32768)
	raw, err := io.ReadAll(io.LimitReader(file, 32769))
	require.NoError(t, err)
	require.True(t, int64(len(raw)) == st.Size)
	return raw
}

func TestDSGSameMountApprovedOperation(t *testing.T) {
	args := os.Args
	if len(args) < 5 || args[len(args)-4] != "--" {
		t.Skip("requires separately provisioned dedicated guest authorization")
	}
	mode, root := args[len(args)-3], dsgCustodyRoot(t, args[len(args)-2])
	require.True(t, mode == "dsg-apply-same-mount" || mode == "dsg-recover-same-mount")
	stage := filepath.Dir(filepath.Dir(root))
	var st unix.Stat_t
	require.NoError(t, unix.Stat(root, &st))
	require.Equal(t, "grant.json", filepath.Base(args[len(args)-1]))
	data := dsgCustodyAuthorityFile(t, args[len(args)-1], uint64(st.Dev))
	var grant dsgSameMountGrant
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	require.NoError(t, decoder.Decode(&grant))
	require.ErrorIs(t, decoder.Decode(new(json.RawMessage)), io.EOF)
	require.True(t, grant.valid())
	custody := dsgCustodyAuthorityFile(t, filepath.Join(stage, "custody.json"), uint64(st.Dev))
	var sealed struct {
		Databases   map[string]string    `json:"native_databases_sha256"`
		Evidence    string               `json:"evidence_sha256"`
		Expires     int64                `json:"expires_at_unix_ms"`
		BootExpires int64                `json:"expires_at_boottime_ms"`
		Runtime     string               `json:"runtime_sha256"`
		Source      string               `json:"native_source_sha256"`
		Authority   dsgSameMountEvidence `json:"authority"`
	}
	require.NoError(t, json.Unmarshal(custody, &sealed))
	require.True(t, grant.Evidence == sealed.Authority && grant.CustodySHA256 == sealed.Evidence && grant.Request.ExpiresAtMS <= sealed.Expires)
	require.Len(t, sealed.Databases, len(dsgSameMountDatabases))
	for n, name := range dsgSameMountDatabases {
		require.True(t, sealed.Databases[name] == grant.Evidence.Record.DatabasesSHA256[n])
	}
	var fields map[string]any
	decoder = json.NewDecoder(bytes.NewReader(custody))
	decoder.UseNumber()
	require.NoError(t, decoder.Decode(&fields))
	delete(fields, "evidence_sha256")
	canonical, err := json.Marshal(fields)
	require.NoError(t, err)
	require.True(t, dsgRecoveryHash(canonical) == grant.CustodySHA256)
	require.True(t, dsgRecoveryHash(dsgCustodyAuthorityFile(t, filepath.Join(stage, "native-source.json"), uint64(st.Dev))) == sealed.Source)
	binary, err := os.Open("/proc/self/exe")
	require.NoError(t, err)
	sum := sha256.New()
	size, err := io.Copy(sum, io.LimitReader(binary, (100<<20)+1))
	closeErr := binary.Close()
	require.NoError(t, err)
	require.NoError(t, closeErr)
	require.True(t, size > 0 && size <= 100<<20 && hex.EncodeToString(sum.Sum(nil)) == sealed.Runtime)
	var clock unix.Timespec
	require.NoError(t, unix.ClockGettime(unix.CLOCK_BOOTTIME, &clock))
	require.True(t, clock.Nano()/1e6 < sealed.BootExpires && time.Now().UnixMilli() < sealed.Expires)
	remaining := min(sealed.BootExpires-clock.Nano()/1e6, sealed.Expires-time.Now().UnixMilli())
	require.True(t, remaining > 0 && remaining <= int64((24*time.Hour)/time.Millisecond))
	ctx, cancel := context.WithTimeout(t.Context(), time.Duration(remaining)*time.Millisecond)
	defer cancel()
	native := dsgSameMountNative{input: dsgCustodyReadInput(t, root)}
	require.True(t, dsgSameMountNativeSource(native.input) == grant.Evidence.Source)
	authority, err := dsgOpenSameMountAuthority(filepath.Join(stage, "grants.db"))
	require.NoError(t, err)
	defer authority.db.Close()
	var got DSGExactReceipt
	if mode == "dsg-apply-same-mount" {
		require.NoError(t, authority.admit(ctx, grant))
		got, err = authority.apply(ctx, root, grant, native, dsgCustodyRemount)
	} else {
		got, err = authority.recover(ctx, root, grant, native, dsgCustodyRemount)
	}
	require.NoError(t, err)
	reply, err := json.Marshal(got)
	require.NoError(t, err)
	t.Logf("DSG_SAME_MOUNT_RECEIPT %s", reply)
}
