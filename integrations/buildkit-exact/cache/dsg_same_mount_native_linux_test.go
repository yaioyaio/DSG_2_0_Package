//go:build linux

package cache

import (
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
	bolt "go.etcd.io/bbolt"
)

// Native index keys contain binary allocation IDs. The non-target witness must
// distinguish different byte sequences even when neither key is valid UTF-8.
func TestDSGSameMountNativeBinaryPeerEvidence(t *testing.T) {
	db, err := bolt.Open(filepath.Join(t.TempDir(), "peers.db"), 0600, nil)
	require.NoError(t, err)
	defer db.Close()
	observe := func(key byte) string {
		t.Helper()
		var result string
		require.NoError(t, db.Update(func(tx *bolt.Tx) error {
			root, err := tx.CreateBucketIfNotExists([]byte("parents"))
			if err != nil {
				return err
			}
			if err = root.Put([]byte{key}, []byte("value")); err != nil {
				return err
			}
			result, err = dsgTrialTree(tx, func([]string) bool { return false })
			if err != nil {
				return err
			}
			return root.Delete([]byte{key})
		}))
		return result
	}
	require.NotEqual(t, observe(0xff), observe(0xfe))
}
