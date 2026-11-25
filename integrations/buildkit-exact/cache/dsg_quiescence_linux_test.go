//go:build linux

package cache

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDSGExactQuiescenceMountTree(t *testing.T) {
	const root = "/trial/native/worker"
	primary := dsgMount{id: "4", parent: "2", device: "7:0", root: "/", path: "/trial/native",
		options: "ro,nodev,nosuid", fs: "ext4", super: "ro", private: true}
	check := func(entries ...dsgMount) error { return dsgCheckMountTree(entries, "7:0", root, true) }
	require.NoError(t, check(primary))
	alias := primary
	alias.id, alias.path = "8", "/elsewhere"
	require.ErrorContains(t, check(alias, primary), "quiescence_mounts")
	require.ErrorContains(t, check(primary, dsgMount{id: "5", parent: "4", device: "0:37"}), "quiescence_mounts")
	writableBackend := primary
	writableBackend.super = "rw"
	require.ErrorContains(t, check(writableBackend), "quiescence_filesystem")
	shared := primary
	shared.private = false
	require.ErrorContains(t, check(shared), "quiescence_filesystem")
	subtree := primary
	subtree.root = "/partial"
	require.ErrorContains(t, check(subtree), "quiescence_filesystem")
}

func TestDSGExactQuiescencePreservesPreviousGeneration(t *testing.T) {
	root := t.TempDir()
	require.NoError(t, os.Chmod(root, 0700))
	require.NoError(t, os.WriteFile(filepath.Join(root, dsgNSMarker), []byte("42"), 0600))
	require.ErrorIs(t, DSGRejectEnrolledRoot(root), ErrDSGExactPreserve)
	for _, start := range []func(string) error{DSGAcquireDedicatedDaemon, DSGAcquireQuiescentDaemon} {
		require.ErrorIs(t, start(root), ErrDSGExactPreserve)
	}
	_, err := os.Lstat(filepath.Join(root, dsgMarker))
	require.ErrorIs(t, err, os.ErrNotExist)
	require.NoError(t, os.WriteFile(filepath.Join(root, dsgMarker), nil, 0600))
	require.ErrorIs(t, DSGAcquireDedicatedDaemon(root), ErrDSGExactPreserve)
}

func TestDSGExactQuiescenceLiveExecutorNamespace(t *testing.T) {
	ns, err := dsgStat("/proc/self/ns/pid")
	require.NoError(t, err)
	scan := dsgProcScan{namespace: ns.Ino, pids: map[uint64]bool{}}
	require.ErrorContains(t, scan.checkPID("/proc/self/ns/pid"), "quiescence_namespace")
	root := t.TempDir()
	require.NoError(t, os.Chmod(root, 0700))
	require.ErrorIs(t, DSGAcquireQuiescentDaemon(root), ErrDSGExactPreserve)
	require.NoFileExists(t, filepath.Join(root, dsgNSMarker))
}
