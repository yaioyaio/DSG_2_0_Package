//go:build linux

package cache

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDSGOperatorExactFrame(t *testing.T) {
	hash := dsgRecoveryHash([]byte("selector"))
	id := strings.Repeat("a", 25)
	wire := map[string]string{
		"project": hash, "worker": id, "record_id": id, "generation": hash, "origin": hash,
		"metadata_digest": hash, "approval": hash, "intent": hash, "custody_sha256": hash,
	}
	raw, err := json.Marshal(wire)
	require.NoError(t, err)
	r, err := DSGDecodeOperatorRequest(bytes.NewReader(raw))
	require.NoError(t, err)
	require.Equal(t, DSGOperatorRequest{hash, id, id, hash, hash, hash, hash, hash, hash}, r)

	for name, input := range map[string]string{
		"trailing":       string(raw) + "{}",
		"missing":        strings.Replace(string(raw), `"worker":"`+id+`"`, `"path":"worker"`, 1),
		"partial":        strings.ReplaceAll(string(raw), id, id[:24]),
		"duplicate":      `{"approval":"` + hash + `",` + string(raw[1:]),
		"unknown":        `{"command":"apply",` + string(raw[1:]),
		"case":           strings.Replace(string(raw), "approval", "Approval", 1),
		"oversize":       string(raw) + strings.Repeat(" ", 4096),
		"invalid_digest": strings.Replace(string(raw), hash, strings.ToUpper(hash), 1),
	} {
		t.Run(name, func(t *testing.T) {
			_, err := DSGDecodeOperatorRequest(strings.NewReader(input))
			require.ErrorIs(t, err, ErrDSGExactPreserve)
		})
	}
}

// Registration and operation names are validated before filesystem access.
func TestDSGOperatorRejectsUnknownRoute(t *testing.T) {
	hash := dsgRecoveryHash([]byte("selector"))
	id := strings.Repeat("a", 25)
	r := DSGOperatorRequest{hash, id, id, hash, hash, hash, hash, hash, hash}
	for _, tc := range [][2]string{{"prune", "0123456789ab"}, {"apply", "../worker"}, {"approve", "/run/buildkit"}, {"receipt", "0123456789AB"}} {
		_, err := DSGRunExactOperator(t.Context(), tc[0], tc[1], r)
		require.ErrorIs(t, err, ErrDSGExactPreserve)
	}
}
