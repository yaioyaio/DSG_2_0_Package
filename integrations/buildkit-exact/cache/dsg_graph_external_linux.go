//go:build linux

package cache

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"

	bolt "go.etcd.io/bbolt"
)

// DSGGraphExternal projects the pinned stores' structural references. GC root
// policy and indirect references are accounted for in the transitive graph.
type DSGGraphExternal struct {
	ImageObjects                int `json:"image_objects"`
	ImageTargetReferences       int `json:"image_target_references"`
	ContainerObjects            int `json:"container_objects"`
	ContainerSnapshotReferences int `json:"container_snapshot_references"`
	IngestObjects               int `json:"ingest_objects"`
	IngestExpectedReferences    int `json:"ingest_expected_references"`
	HistoryRecords              int `json:"history_records"`
	HistoryDescriptors          int `json:"history_descriptors"`
	HistoryDescriptorReferences int `json:"history_descriptor_references"`
	OpaqueHistoryRecords        int `json:"opaque_history_records"`
	CacheFileLists              int `json:"cache_file_lists"`
	CacheContentHashes          int `json:"cache_content_hashes"`
	CacheOpaqueEntries          int `json:"cache_opaque_entries"`
	TargetCacheExternalEntries  int `json:"target_cache_external_entries"`
}

func (r *dsgGraphReader) externalRefs(cacheTx, containerdTx, historyTx *bolt.Tx, recordID, snapshot, blob string) (*DSGGraphExternal, []string) {
	o := &DSGGraphExternal{}
	var unverified []string
	if err := r.externalContainerd(o, containerdTx, snapshot, blob); err != nil {
		unverified = append(unverified, "containerd_image_container_or_ingest_projection")
	}
	if err := r.externalHistory(o, historyTx, blob); err != nil {
		unverified = append(unverified, "build_history_projection")
	}
	if o.OpaqueHistoryRecords != 0 {
		unverified = append(unverified, "opaque_build_history_records")
	}
	if err := r.externalCache(o, cacheTx, recordID); err != nil {
		unverified = append(unverified, "external_cache_metadata_projection")
	}
	if o.CacheOpaqueEntries != 0 {
		unverified = append(unverified, "opaque_external_cache_metadata")
	}
	return o, unverified
}

type dsgExternalStructural struct {
	blob, snapshotter, snapshot string
}

// dsgReadExternalStructural mirrors the three built-in object branches of
// containerd v2.4.2 metadata.gcContext.references. Store names, specs, image
// names in containers, ingest refs and arbitrary metadata are left untouched.
func dsgReadExternalStructural(kind string, b *bolt.Bucket) (dsgExternalStructural, error) {
	var ref dsgExternalStructural
	if b == nil {
		return ref, ErrDSGExactPreserve
	}
	var err error
	switch kind {
	case "image":
		target, bucketErr := dsgGraphBucket(b, "target")
		if bucketErr != nil || target == nil {
			return ref, ErrDSGExactPreserve
		}
		ref.blob, err = dsgExternalValue(target, "digest", 71)
		if !dsgGraphSHA256(ref.blob) {
			return ref, ErrDSGExactPreserve
		}
	case "ingest":
		ref.blob, err = dsgExternalValue(b, "expected", 71)
		if ref.blob != "" && !dsgGraphSHA256(ref.blob) {
			return ref, ErrDSGExactPreserve
		}
	case "container":
		ref.snapshotter, err = dsgExternalValue(b, "snapshotter", 128)
		if err != nil {
			return ref, err
		}
		ref.snapshot, err = dsgExternalValue(b, "snapshotKey", 1024)
		if (ref.snapshotter == "") != (ref.snapshot == "") {
			return ref, ErrDSGExactPreserve
		}
	default:
		return ref, ErrDSGExactPreserve
	}
	return ref, err
}

func dsgExternalValue(b *bolt.Bucket, key string, maxLen int) (string, error) {
	k := []byte(key)
	if b.Bucket(k) != nil {
		return "", ErrDSGExactPreserve
	}
	v := b.Get(k)
	if len(v) > maxLen {
		return "", ErrDSGExactPreserve
	}
	return string(v), nil
}

func dsgExternalNative(ns *bolt.Bucket) (*bolt.Bucket, error) {
	snapshotters, err := dsgGraphBucket(ns, "snapshots")
	if err != nil {
		return nil, err
	}
	return dsgGraphBucket(snapshotters, "native")
}

func (r *dsgGraphReader) externalContainerd(o *DSGGraphExternal, tx *bolt.Tx, snapshot, blob string) error {
	if tx == nil || tx.Writable() || !dsgGraphSnapshotKey(snapshot) || (blob != "" && !dsgGraphSHA256(blob)) {
		return ErrDSGExactPreserve
	}
	if err := tx.ForEach(func(k []byte, _ *bolt.Bucket) error {
		if string(k) != "v1" {
			return ErrDSGExactPreserve
		}
		return nil
	}); err != nil {
		return err
	}
	root := tx.Bucket([]byte("v1"))
	if root == nil {
		return ErrDSGExactPreserve
	}
	version := root.Get([]byte("version"))
	n, size := binary.Varint(version)
	if n != 4 || size != len(version) {
		return ErrDSGExactPreserve
	}
	buildkit, err := dsgGraphBucket(root, dsgGraphNamespace)
	if err != nil {
		return err
	}
	native, err := dsgExternalNative(buildkit)
	if err != nil {
		return err
	}
	mapping, err := dsgGraphBucket(native, snapshot)
	if err != nil {
		return err
	}
	var backing string
	if mapping != nil {
		backing, err = dsgExternalValue(mapping, "name", 128)
		if err != nil || !dsgNativeKey(backing) {
			return ErrDSGExactPreserve
		}
	}
	return r.each(root, func(nsKey, v []byte) error {
		if string(nsKey) == "version" {
			return nil
		}
		if v != nil || len(nsKey) > 128 {
			return ErrDSGExactPreserve
		}
		ns := root.Bucket(nsKey)
		content, err := dsgGraphBucket(ns, "content")
		if err != nil {
			return err
		}
		native, err := dsgExternalNative(ns)
		if err != nil {
			return err
		}
		for _, objects := range []struct {
			parent       *bolt.Bucket
			key, kind    string
			count, match *int
		}{
			{ns, "images", "image", &o.ImageObjects, &o.ImageTargetReferences},
			{ns, "containers", "container", &o.ContainerObjects, &o.ContainerSnapshotReferences},
			{content, "ingests", "ingest", &o.IngestObjects, &o.IngestExpectedReferences},
		} {
			b, err := dsgGraphBucket(objects.parent, objects.key)
			if err != nil {
				return err
			}
			err = r.each(b, func(k, v []byte) error {
				if v != nil {
					return ErrDSGExactPreserve
				}
				(*objects.count)++
				ref, err := dsgReadExternalStructural(objects.kind, b.Bucket(k))
				if err != nil {
					return err
				}
				target, alias := ref.blob, ""
				matched := target != "" && target == blob
				if ref.snapshotter == "native" {
					target = snapshot
					matched, err = dsgExternalSnapshotMatch(native, string(nsKey), ref.snapshot, snapshot, backing)
					if err != nil {
						return err
					}
					if matched {
						alias = dsgExternalIdentifier([]byte(ref.snapshot))
					}
				}
				if matched {
					(*objects.match)++
					r.edge("external-object", objects.kind, dsgExternalIdentifier(nsKey), dsgExternalIdentifier(k), target, alias)
				}
				return nil
			})
			if err != nil {
				return err
			}
		}
		return nil
	})
}

func dsgExternalSnapshotMatch(native *bolt.Bucket, ns, key, target, backing string) (bool, error) {
	if ns == dsgGraphNamespace && key == target {
		return true, nil
	}
	alias, err := dsgGraphBucket(native, key)
	if err != nil || alias == nil || backing == "" {
		return false, err
	}
	name, err := dsgExternalValue(alias, "name", 1024)
	if err != nil || name == "" {
		return false, ErrDSGExactPreserve
	}
	return backing == name, nil
}

// Object keys are not part of the public projection. A digest binds identity
// changes into aggregate evidence without adding the names to an edge.
func dsgExternalIdentifier(key []byte) string {
	sum := sha256.Sum256(key)
	return hex.EncodeToString(sum[:])
}
