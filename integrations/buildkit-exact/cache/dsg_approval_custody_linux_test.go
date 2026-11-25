//go:build linux

package cache

import (
	"bytes"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"testing"

	"github.com/stretchr/testify/require"
	"golang.org/x/sys/unix"
)

var dsgCustodyPath = regexp.MustCompile(`^/run/dsg-approval-[0-9a-f]{12,64}/stage/candidate/worker$`)

func dsgCustodyRoot(t *testing.T, path string) string {
	t.Helper()
	require.True(t, os.Geteuid() == 0 && dsgCustodyPath.MatchString(path))
	resolved, err := filepath.EvalSymlinks(path)
	require.True(t, err == nil && resolved == path)
	for _, dir := range []string{path, filepath.Dir(filepath.Dir(path))} {
		var info unix.Stat_t
		require.NoError(t, unix.Lstat(dir, &info))
		require.True(t, info.Uid == 0 && info.Mode == unix.S_IFDIR|0700)
	}
	return path
}

func dsgCustodyReadInput(t *testing.T, root string) dsgReclaimInput {
	t.Helper()
	path := filepath.Join(filepath.Dir(filepath.Dir(root)), "native-source.json")
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_CLOEXEC|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	require.NoError(t, err)
	file := os.NewFile(uintptr(fd), "native-source.json")
	defer file.Close()
	var info unix.Stat_t
	require.NoError(t, unix.Fstat(fd, &info))
	require.True(t, info.Uid == 0 && info.Mode == unix.S_IFREG|0600 && info.Nlink == 1)
	require.True(t, info.Size > 0 && info.Size <= 16*1024)
	data, err := io.ReadAll(io.LimitReader(file, info.Size+1))
	require.NoError(t, err)
	require.True(t, int64(len(data)) == info.Size)
	var input dsgReclaimInput
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	require.NoError(t, decoder.Decode(&input))
	require.ErrorIs(t, decoder.Decode(new(json.RawMessage)), io.EOF)
	return input
}

func TestDSGNativeSameMountCustody(t *testing.T) {
	args := os.Args
	if len(args) < 4 || args[len(args)-3] != "--" {
		t.Skip("requires a dedicated private ext4 mount")
	}
	mode, root := args[len(args)-2], dsgCustodyRoot(t, args[len(args)-1])
	switch mode {
	case "dsg-produce-approval":
		require.Equal(t, filepath.Join(filepath.Dir(root), "producer-tmp"), os.TempDir())
		entries, err := os.ReadDir(root)
		require.NoError(t, err)
		require.Empty(t, entries)
		// Keep the original ext4 mount in the keeper's namespace. A separate
		// procfs at this private staging path exposes the producer as PID 1.
		proc := filepath.Join(filepath.Dir(filepath.Dir(root)), "producer-proc")
		var info unix.Stat_t
		require.NoError(t, unix.Lstat(proc, &info))
		require.True(t, os.Getpid() == 1 && info.Uid == 0 && info.Mode == unix.S_IFDIR|0700)
		require.NoError(t, unix.Mount("proc", proc, "proc", unix.MS_NOSUID|unix.MS_NODEV|unix.MS_NOEXEC, ""))
		defer func() { require.NoError(t, unix.Unmount(proc, 0)) }()
		require.NoError(t, dsgAcquireDaemonFromProc(root, true, proc))
		_, input := dsgSeedReclaimAt(t, filepath.Join(root, dsgDeletionRoot))
		data, err := json.Marshal(struct {
			Source dsgSameMountSource `json:"source"`
			Input  dsgReclaimInput    `json:"input"`
		}{dsgSameMountNativeSource(input), input})
		require.NoError(t, err)
		require.LessOrEqual(t, len(data), 16*1024)
		t.Logf("DSG_APPROVAL_PRODUCED %s", data)
	case "dsg-inspect-approval":
		input := dsgCustodyReadInput(t, root)
		evidence, err := dsgInspectSameMount(t.Context(), root, dsgSameMountNativeSource(input), dsgSameMountNative{input: input})
		require.NoError(t, err)
		data, err := json.Marshal(evidence)
		require.NoError(t, err)
		require.LessOrEqual(t, len(data), 16*1024)
		t.Logf("DSG_APPROVAL_INSPECTED %s", data)
	default:
		t.Fatal("unsupported custody operation")
	}
}
