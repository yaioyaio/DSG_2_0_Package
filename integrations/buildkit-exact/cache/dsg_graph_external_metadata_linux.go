//go:build linux

package cache

import (
	"bytes"
	"encoding/json"

	bolt "go.etcd.io/bbolt"
	"google.golang.org/protobuf/encoding/protowire"
)

func (r *dsgGraphReader) externalCache(o *DSGGraphExternal, tx *bolt.Tx, target string) error {
	if tx == nil || tx.Writable() || !dsgFullID.MatchString(target) {
		return ErrDSGExactPreserve
	}
	external := tx.Bucket([]byte("_external"))
	if external == nil {
		return nil
	}
	main := tx.Bucket([]byte("_main"))
	if main == nil {
		return ErrDSGExactPreserve
	}
	// Where possible, only the payload shape is inspected. Filenames, link
	// destinations and contenthash digests are private and have no graph edges.
	remainingBytes := 8 << 20
	return r.each(external, func(id, v []byte) error {
		if v != nil || !dsgFullID.Match(id) {
			return ErrDSGExactPreserve
		}
		owner := main.Bucket(id) != nil
		return r.each(external.Bucket(id), func(k, v []byte) error {
			isTarget := string(id) == target
			if isTarget {
				o.TargetCacheExternalEntries++
			}
			name := string(k)
			if !owner || v == nil || len(v) > dsgExternalPayloadLimit || (name != keyFileList && name != "buildkit.contenthash.v0") {
				o.CacheOpaqueEntries++
				return nil
			}
			remainingBytes -= len(v)
			if remainingBytes < 0 {
				return ErrDSGExactPreserve
			}
			switch name {
			case keyFileList:
				if dsgExternalFileList(v) {
					o.CacheFileLists++
					if isTarget {
						r.edge("external-cache-format", target, name)
					}
					return nil
				}
			case "buildkit.contenthash.v0":
				if r.externalContentHash(v) == nil {
					o.CacheContentHashes++
					if isTarget {
						r.edge("external-cache-format", target, name)
					}
					return nil
				}
			}
			o.CacheOpaqueEntries++
			return nil
		})
	})
}

// A pinned filelist is []string or null. Let the standard validator handle
// escapes and framing, then inspect the token types without retaining paths.
func dsgExternalFileList(v []byte) bool {
	if !json.Valid(v) {
		return false
	}
	v = bytes.TrimSpace(v)
	if bytes.Equal(v, []byte("null")) {
		return true
	}
	if v[0] != '[' {
		return false
	}
	v = bytes.TrimSpace(v[1:])
	for v[0] != ']' {
		if v[0] != '"' {
			return false
		}
		end := 1
		for v[end] != '"' {
			if v[end] == '\\' {
				end++
			}
			end++
		}
		v = bytes.TrimSpace(v[end+1:])
		if v[0] == ',' {
			v = bytes.TrimSpace(v[1:])
		}
	}
	return true
}

// cache/contenthash/checksum.proto describes a radix-tree path index, with
// local file hashes and symlink targets. It carries no storage references.
func (r *dsgGraphReader) externalContentHash(raw []byte) error {
	return r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		if n != 1 || typ != protowire.BytesType {
			return ErrDSGExactPreserve
		}
		var seen uint8
		err := r.externalWire(v, func(n protowire.Number, typ protowire.Type, v []byte) error {
			if n > 2 || typ != protowire.BytesType || seen&(1<<n) != 0 {
				return ErrDSGExactPreserve
			}
			seen |= 1 << n
			if n == 2 {
				return r.externalContentHashRecord(v)
			}
			return nil
		})
		if err != nil || seen&(1<<2) == 0 {
			return ErrDSGExactPreserve
		}
		return nil
	})
}

func (r *dsgGraphReader) externalContentHashRecord(raw []byte) error {
	var seen uint8
	return r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		if n > 3 || seen&(1<<n) != 0 {
			return ErrDSGExactPreserve
		}
		seen |= 1 << n
		if n == 2 {
			t, _ := protowire.ConsumeVarint(v)
			if typ != protowire.VarintType || t > 3 {
				return ErrDSGExactPreserve
			}
		} else if typ != protowire.BytesType {
			return ErrDSGExactPreserve
		}
		return nil
	})
}
