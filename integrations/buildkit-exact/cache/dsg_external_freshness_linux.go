//go:build linux

package cache

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"io"
	"net"
	"path/filepath"
	"time"

	"golang.org/x/sys/unix"
)

const dsgFreshnessSocket = "/run/dsg-buildkit-freshness/authority.sock"

// The socket authenticates the local root transport. Snapshot independence
// requires its upstream authority to keep its state outside the worker VM.
func dsgClaimExternalFreshness(ctx context.Context, registration string, g dsgSameMountGrant, checkpoint string) error {
	if !dsgRegistrationID.MatchString(registration) || !dsgDigest.MatchString(checkpoint) ||
		!dsgDigest.MatchString(g.CustodySHA256) || !dsgDigest.MatchString(g.Request.Intent) || !dsgDigest.MatchString(g.Request.Approval) {
		return ErrDSGExactPreserve
	}
	var dir, socket unix.Stat_t
	if !dsgProtectedParent(dsgFreshnessSocket) || unix.Lstat(filepath.Dir(dsgFreshnessSocket), &dir) != nil ||
		dir.Uid != 0 || dir.Mode != unix.S_IFDIR|0700 || unix.Lstat(dsgFreshnessSocket, &socket) != nil ||
		socket.Uid != 0 || socket.Mode != unix.S_IFSOCK|0600 {
		return ErrDSGExactPreserve
	}
	ctx, cancel := context.WithTimeout(ctx, 4*time.Second)
	defer cancel()
	c, err := (&net.Dialer{}).DialContext(ctx, "unix", dsgFreshnessSocket)
	if err != nil {
		return ErrDSGExactPreserve
	}
	defer c.Close()
	conn := c.(*net.UnixConn)
	raw, err := conn.SyscallConn()
	if err != nil {
		return ErrDSGExactPreserve
	}
	var trusted bool
	if err := raw.Control(func(fd uintptr) {
		cred, err := unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED)
		trusted = err == nil && cred.Uid == 0
	}); err != nil || !trusted {
		return ErrDSGExactPreserve
	}
	return dsgExchangeFreshness(ctx, conn, registration, g, checkpoint)
}

func dsgExchangeFreshness(ctx context.Context, conn *net.UnixConn, registration string, g dsgSameMountGrant, checkpoint string) error {
	deadline, _ := ctx.Deadline()
	if conn.SetDeadline(deadline) != nil {
		return ErrDSGExactPreserve
	}
	stop := context.AfterFunc(ctx, func() { conn.Close() })
	defer stop()
	scope := dsgRecoveryHash([]byte(registration + "\n" + g.CustodySHA256 + "\n" + g.Request.Intent + "\n" + g.Request.Approval + "\n"))
	var challenge [32]byte
	if _, err := rand.Read(challenge[:]); err != nil {
		return ErrDSGExactPreserve
	}
	nonce := hex.EncodeToString(challenge[:])
	request := "DSG-FRESHNESS-1 " + registration + " " + scope + " " + checkpoint + " " + nonce + "\n"
	if n, err := io.WriteString(conn, request); err != nil || n != len(request) || conn.CloseWrite() != nil {
		return ErrDSGExactPreserve
	}
	expected := "DSG-FRESHNESS-1 CLAIMED " + nonce + "\n"
	reply, err := io.ReadAll(io.LimitReader(conn, int64(len(expected)+1)))
	if err != nil || string(reply) != expected || ctx.Err() != nil {
		return ErrDSGExactPreserve
	}
	return nil
}
