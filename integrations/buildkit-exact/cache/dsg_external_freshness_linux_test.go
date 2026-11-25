//go:build linux

package cache

import (
	"context"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestDSGExternalFreshnessWire(t *testing.T) {
	g := dsgTestLedgerGrant(1)
	registration := "0123456789ab"
	checkpoint := strings.Repeat("c", 64)
	scope := dsgRecoveryHash([]byte(strings.Join([]string{registration, g.CustodySHA256, g.Request.Intent, g.Request.Approval, ""}, "\n")))
	for _, mode := range []string{"exact", "denied", "wrong-nonce", "trailing", "truncated"} {
		t.Run(mode, func(t *testing.T) {
			server, err := net.ListenUnix("unix", &net.UnixAddr{Name: filepath.Join(t.TempDir(), "a"), Net: "unix"})
			require.NoError(t, err)
			defer server.Close()
			conn, err := net.DialUnix("unix", nil, server.Addr().(*net.UnixAddr))
			require.NoError(t, err)
			defer conn.Close()
			peer, err := server.AcceptUnix()
			require.NoError(t, err)
			defer peer.Close()
			ctx, cancel := context.WithTimeout(t.Context(), 4*time.Second)
			defer cancel()
			deadline, _ := ctx.Deadline()
			require.NoError(t, peer.SetDeadline(deadline))
			done := make(chan error, 1)
			go func() { done <- dsgExchangeFreshness(ctx, conn, registration, g, checkpoint) }()
			request, err := io.ReadAll(io.LimitReader(peer, 512))
			require.NoError(t, err)
			fields := strings.Split(strings.TrimSuffix(string(request), "\n"), " ")
			require.Len(t, fields, 5)
			require.Equal(t, []string{"DSG-FRESHNESS-1", registration, scope, checkpoint}, fields[:4])
			require.Regexp(t, dsgDigest, fields[4])
			require.True(t, strings.HasSuffix(string(request), "\n"))
			reply := "DSG-FRESHNESS-1 CLAIMED " + fields[4] + "\n"
			switch mode {
			case "denied":
				reply = "DSG-FRESHNESS-1 DENIED\n"
			case "wrong-nonce":
				reply = "DSG-FRESHNESS-1 CLAIMED " + strings.Repeat("0", 64) + "\n"
			case "trailing":
				reply += "\n"
			case "truncated":
				reply = strings.TrimSuffix(reply, "\n")
			}
			_, err = io.WriteString(peer, reply)
			require.NoError(t, err)
			require.NoError(t, peer.CloseWrite())
			if mode == "exact" {
				require.NoError(t, <-done)
			} else {
				require.ErrorIs(t, <-done, ErrDSGExactPreserve)
			}
		})
	}
}

func TestDSGExternalFreshnessApprovedCheckpoint(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	require.NoError(t, os.Chmod(dir, 0700))
	a, err := dsgCreateSameMountAuthority(filepath.Join(dir, "grants.db"))
	require.NoError(t, err)
	defer a.db.Close()
	g := dsgTestLedgerGrant(a.device + 1)
	calls := 0
	allow := false
	a.claimFreshness = func(_ context.Context, grant dsgSameMountGrant, checkpoint string) error {
		calls++
		require.Equal(t, g, grant)
		require.Equal(t, string(a.currentDigest()), checkpoint)
		if !allow {
			return ErrDSGExactPreserve
		}
		return nil
	}
	_, err = a.claimApprovedCheckpoint(ctx, g)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.Zero(t, calls)
	require.NoError(t, a.approve(ctx, g))
	before := a.currentDigest()
	_, err = a.claimApprovedCheckpoint(ctx, g)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.Equal(t, before, a.currentDigest())
	allow = true
	got, err := a.claimApprovedCheckpoint(ctx, g)
	require.NoError(t, err)
	require.Equal(t, before, got)
	require.ErrorIs(t, a.advanceFrom(ctx, g, nil, []byte("stale")), ErrDSGExactPreserve)
	require.Equal(t, before, a.currentDigest())
	require.NoError(t, a.advanceFrom(ctx, g, nil, got))
	_, err = a.claimApprovedCheckpoint(ctx, g)
	require.ErrorIs(t, err, ErrDSGExactPreserve)
	require.Equal(t, 2, calls)
}
