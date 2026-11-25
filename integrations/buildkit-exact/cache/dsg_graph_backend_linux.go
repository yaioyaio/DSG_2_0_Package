//go:build linux

package cache

import (
	"bytes"
	"encoding/binary"
	"strconv"
	"strings"

	"github.com/containerd/containerd/v2/core/snapshots"
	bolt "go.etcd.io/bbolt"
)

const dsgGraphNamespace = "buildkit"

type DSGGraphBackend struct {
	Namespaces            int  `json:"namespaces"`
	RecordLease           bool `json:"record_lease"`
	VariantLease          bool `json:"variant_lease"`
	ViewLease             bool `json:"view_lease"`
	RecordLeaseSnapshots  int  `json:"record_lease_snapshots"`
	RecordLeaseBlobs      int  `json:"record_lease_blobs"`
	VariantLeaseSnapshots int  `json:"variant_lease_snapshots"`
	VariantLeaseBlobs     int  `json:"variant_lease_blobs"`
	ViewLeaseSnapshots    int  `json:"view_lease_snapshots"`
	ViewLeaseBlobs        int  `json:"view_lease_blobs"`
	OtherOwnedResources   int  `json:"other_owned_lease_resources"`
	TargetLeaseSnapshot   bool `json:"target_snapshot_in_record_lease"`
	TargetLeaseBlob       bool `json:"target_blob_in_record_lease"`
	SnapshotLeases        int  `json:"snapshot_leases"`
	BlobLeases            int  `json:"blob_leases"`
	SnapshotMapping       bool `json:"snapshot_mapping"`
	SnapshotParents       int  `json:"snapshot_parents"`
	SnapshotChildren      int  `json:"snapshot_children"`
	NativeSnapshotAliases int  `json:"native_snapshot_aliases"`
	BlobNamespaces        int  `json:"blob_namespaces"`
}

type DSGGraphSnapshot struct {
	Kind             string `json:"kind"`
	Parents          int    `json:"parents"`
	Children         int    `json:"children"`
	DirectoryPresent bool   `json:"directory_present"`
}

type DSGGraphBlob struct {
	Size int64 `json:"size"`
}

type dsgGraphNativeKeys struct {
	key, parent string
}

func dsgNativeKey(value string) bool {
	ns, tail, ok := strings.Cut(value, "/")
	if !ok || ns != dsgGraphNamespace {
		return false
	}
	sequence, name, ok := strings.Cut(tail, "/")
	n, err := strconv.ParseUint(sequence, 10, 64)
	return ok && err == nil && n != 0 && sequence == strconv.FormatUint(n, 10) && dsgGraphSnapshotKey(name)
}

func (r *dsgGraphReader) containerd(o *DSGDedicatedGraph, tx *bolt.Tx, id, snapshot, blob string) (dsgGraphNativeKeys, *int64, error) {
	var keys dsgGraphNativeKeys
	if tx == nil {
		return keys, nil, ErrDSGExactPreserve
	}
	root := tx.Bucket([]byte("v1"))
	if root == nil {
		return keys, nil, ErrDSGExactPreserve
	}
	rawVersion := root.Get([]byte("version"))
	version, numBytes := binary.Varint(rawVersion)
	if numBytes != len(rawVersion) || version != 4 {
		return keys, nil, ErrDSGExactPreserve
	}
	out := &DSGGraphBackend{}
	buildkit := root.Bucket([]byte(dsgGraphNamespace))
	native := dsgGraphNativeBucket(buildkit)
	if native != nil {
		record, err := dsgGraphBucket(native, snapshot)
		if err != nil {
			return keys, nil, err
		}
		if record != nil {
			keys.key = string(record.Get([]byte("name")))
			if !dsgNativeKey(keys.key) {
				return keys, nil, ErrDSGExactPreserve
			}
			r.edge("containerd-snapshot-map", snapshot, keys.key)
			out.SnapshotMapping = true
			parent := record.Get([]byte("parent"))
			if len(parent) != 0 {
				if !dsgGraphSnapshotKey(string(parent)) || string(parent) == snapshot {
					return keys, nil, ErrDSGExactPreserve
				}
				item := native.Bucket(parent)
				if item == nil {
					return keys, nil, ErrDSGExactPreserve
				}
				keys.parent = string(item.Get([]byte("name")))
				if !dsgNativeKey(keys.parent) || keys.parent == keys.key {
					return keys, nil, ErrDSGExactPreserve
				}
				out.SnapshotParents++
				r.edge("containerd-snapshot-parent", string(parent), keys.parent)
			}
		}
	}

	var blobSize *int64
	err := r.each(root, func(k, v []byte) error {
		if v != nil {
			if string(k) == "version" {
				return nil
			}
			return ErrDSGExactPreserve
		}
		if root.Bucket(k) == nil || len(k) > 128 {
			return ErrDSGExactPreserve
		}
		ns := root.Bucket(k)
		isBuildkit := string(k) == dsgGraphNamespace
		out.Namespaces++
		if err := r.containerdSnapshotPeers(out, dsgGraphNativeBucket(ns), snapshot, keys.key, isBuildkit); err != nil {
			return err
		}
		if err := r.leases(out, ns, id, snapshot, blob, isBuildkit); err != nil {
			return err
		}
		if blob != "" {
			content := ns.Bucket([]byte("content"))
			blobs, err := dsgGraphBucket(content, "blob")
			if err != nil {
				return err
			}
			entry, err := dsgGraphBucket(blobs, blob)
			if err != nil {
				return err
			}
			if entry != nil {
				out.BlobNamespaces++
				raw := entry.Get([]byte("size"))
				n, numBytes := binary.Varint(raw)
				if numBytes <= 0 || numBytes != len(raw) || n < 0 {
					return ErrDSGExactPreserve
				}
				if isBuildkit {
					blobSize = &n
				}
				r.edge("containerd-content", strconv.FormatBool(isBuildkit), blob, strconv.FormatInt(n, 10))
			}
		}
		return nil
	})
	if err != nil {
		return keys, nil, err
	}
	o.Containerd = out
	return keys, blobSize, nil
}

func dsgGraphNativeBucket(ns *bolt.Bucket) *bolt.Bucket {
	if ns != nil {
		if parent := ns.Bucket([]byte("snapshots")); parent != nil {
			return parent.Bucket([]byte("native"))
		}
	}
	return nil
}

func (r *dsgGraphReader) containerdSnapshotPeers(out *DSGGraphBackend, b *bolt.Bucket, snapshot, native string, isBuildkit bool) error {
	return r.each(b, func(k, v []byte) error {
		if v != nil {
			return ErrDSGExactPreserve
		}
		if isBuildkit && string(k) == snapshot {
			return nil
		}
		peer := b.Bucket(k)
		if native != "" && string(peer.Get([]byte("name"))) == native {
			if !dsgGraphSnapshotKey(string(k)) {
				return ErrDSGExactPreserve
			}
			out.NativeSnapshotAliases++
			r.edge("containerd-native-alias", strconv.FormatBool(isBuildkit), string(k))
		}
		if isBuildkit && string(peer.Get([]byte("parent"))) == snapshot {
			if !dsgGraphSnapshotKey(string(k)) {
				return ErrDSGExactPreserve
			}
			out.SnapshotChildren++
			r.edge("containerd-snapshot-child", string(k))
		}
		return nil
	})
}

func (r *dsgGraphReader) leases(out *DSGGraphBackend, ns *bolt.Bucket, id, s, blob string, isBuildkit bool) error {
	leases, err := dsgGraphBucket(ns, "leases")
	if err != nil {
		return err
	}
	return r.each(leases, func(k, v []byte) error {
		if v != nil || len(k) > 256 {
			return ErrDSGExactPreserve
		}
		leaseID := string(k)
		role := "other"
		var snapshotCount, blobCount *int
		if isBuildkit {
			switch leaseID {
			case id:
				role = "record"
				out.RecordLease = true
				snapshotCount, blobCount = &out.RecordLeaseSnapshots, &out.RecordLeaseBlobs
			case id + "-variants":
				role = "variants"
				out.VariantLease = true
				snapshotCount, blobCount = &out.VariantLeaseSnapshots, &out.VariantLeaseBlobs
			case id + "-view":
				role = "view"
				out.ViewLease = true
				snapshotCount, blobCount = &out.ViewLeaseSnapshots, &out.ViewLeaseBlobs
			}
		}
		lease := leases.Bucket(k)
		content, err := dsgGraphBucket(lease, "content")
		if err != nil {
			return err
		}
		if err := r.leaseResources(content, blob, role, "blob", blobCount, &out.BlobLeases); err != nil {
			return err
		}
		top, err := dsgGraphBucket(lease, "snapshots")
		if err != nil {
			return err
		}
		err = r.each(top, func(k, v []byte) error {
			if v != nil {
				return ErrDSGExactPreserve
			}
			var incoming *int
			snapshotter := string(k)
			if isBuildkit && snapshotter == "native" {
				incoming = &out.SnapshotLeases
			}
			if snapshotter != "native" {
				if snapshotCount != nil {
					return ErrDSGExactPreserve
				}
				return nil
			}
			return r.leaseResources(top.Bucket(k), s, role, "snapshot", snapshotCount, incoming)
		})
		if err != nil {
			return err
		}
		if role == "record" {
			out.TargetLeaseBlob = blob != "" && content != nil && content.Get([]byte(blob)) != nil
			native, err := dsgGraphBucket(top, "native")
			if err != nil {
				return err
			}
			out.TargetLeaseSnapshot = native != nil && native.Get([]byte(s)) != nil
		}
		if role != "other" {
			for _, key := range []string{"images", "ingests"} {
				b, err := dsgGraphBucket(lease, key)
				if err != nil {
					return err
				}
				if err := r.each(b, func(_, v []byte) error {
					if v == nil || len(v) != 0 {
						return ErrDSGExactPreserve
					}
					out.OtherOwnedResources++
					return nil
				}); err != nil {
					return err
				}
			}
		}
		return nil
	})
}

// Target namespaces and lease roles are recorded without reading auxiliary
// labels or reporting unrelated lease and namespace names.
func (r *dsgGraphReader) leaseResources(b *bolt.Bucket, target, role, kind string, own, incoming *int) error {
	return r.each(b, func(k, v []byte) error {
		if v == nil || len(v) != 0 {
			return ErrDSGExactPreserve
		}
		resource := string(k)
		if own != nil {
			if (kind == "snapshot" && !dsgGraphSnapshotKey(resource)) ||
				(kind == "blob" && !dsgGraphSHA256(resource)) {
				return ErrDSGExactPreserve
			}
			(*own)++
			r.edge("lease-owned-"+kind, role, resource)
		}
		if incoming != nil && target != "" && resource == target {
			(*incoming)++
			r.edge("lease-in-"+kind, role, resource)
		}
		return nil
	})
}

type dsgGraphNativeSnapshot struct {
	id     uint64
	parent string
	kind   snapshots.Kind
}

func dsgGraphReadNativeSnapshot(b *bolt.Bucket) (dsgGraphNativeSnapshot, error) {
	var s dsgGraphNativeSnapshot
	if b == nil {
		return s, ErrDSGExactPreserve
	}
	if b.Bucket([]byte("parent")) != nil {
		return s, ErrDSGExactPreserve
	}
	rawID := b.Get([]byte("id"))
	id, n := binary.Uvarint(rawID)
	kind := b.Get([]byte("kind"))
	if n <= 0 || n != len(rawID) || id == 0 || len(kind) != 1 || !bytes.Equal(rawID, binary.AppendUvarint(nil, id)) {
		return s, ErrDSGExactPreserve
	}
	s = dsgGraphNativeSnapshot{id: id, parent: string(b.Get([]byte("parent"))), kind: snapshots.Kind(kind[0])}
	if s.kind != snapshots.KindActive && s.kind != snapshots.KindCommitted && s.kind != snapshots.KindView {
		return s, ErrDSGExactPreserve
	}
	return s, nil
}

func dsgGraphNativeIndexKey(parent, child uint64) []byte {
	key := binary.AppendUvarint(nil, parent)
	return binary.AppendUvarint(append(key, 0), child)
}

func (r *dsgGraphReader) snapshot(o *DSGDedicatedGraph, tx *bolt.Tx, fd int, keys dsgGraphNativeKeys) error {
	if tx == nil {
		return ErrDSGExactPreserve
	}
	version := tx.Bucket([]byte("v1"))
	records, err := dsgGraphBucket(version, "snapshots")
	if err != nil || records == nil {
		return ErrDSGExactPreserve
	}
	index, err := dsgGraphBucket(version, "parents")
	if err != nil || index == nil {
		return ErrDSGExactPreserve
	}
	target, err := dsgGraphReadNativeSnapshot(records.Bucket([]byte(keys.key)))
	if err != nil || target.parent != keys.parent {
		return ErrDSGExactPreserve
	}
	out := &DSGGraphSnapshot{Kind: target.kind.String()}
	children := make(map[uint64]string)
	var expectedParent uint64
	if target.parent != "" {
		item, err := dsgGraphReadNativeSnapshot(records.Bucket([]byte(target.parent)))
		if err != nil || item.id == target.id {
			return ErrDSGExactPreserve
		}
		expectedParent = item.id
		if string(index.Get(dsgGraphNativeIndexKey(item.id, target.id))) != keys.key {
			return ErrDSGExactPreserve
		}
		out.Parents++
		r.edge("native-snapshot-parent", target.parent, strconv.FormatUint(item.id, 10))
	}
	err = r.each(records, func(k, v []byte) error {
		if v != nil {
			return ErrDSGExactPreserve
		}
		key := string(k)
		if key == keys.key {
			return nil
		}
		item, err := dsgGraphReadNativeSnapshot(records.Bucket(k))
		if err != nil || item.id == target.id {
			return ErrDSGExactPreserve
		}
		if item.parent == keys.key {
			if _, duplicate := children[item.id]; duplicate || !dsgNativeKey(key) ||
				string(index.Get(dsgGraphNativeIndexKey(target.id, item.id))) != key {
				return ErrDSGExactPreserve
			}
			children[item.id] = key
			out.Children++
			r.edge("native-snapshot-child", key, strconv.FormatUint(item.id, 10))
		}
		return nil
	})
	if err != nil {
		return err
	}
	err = r.each(index, func(k, v []byte) error {
		parent, n := binary.Uvarint(k)
		if n <= 0 || n+1 >= len(k) || k[n] != 0 || v == nil || len(v) > 256 {
			return ErrDSGExactPreserve
		}
		child, m := binary.Uvarint(k[n+1:])
		if m <= 0 || n+1+m != len(k) {
			return ErrDSGExactPreserve
		}
		if !bytes.Equal(k, dsgGraphNativeIndexKey(parent, child)) || (string(v) == keys.key && child != target.id) ||
			(child == target.id && (parent != expectedParent || string(v) != keys.key)) ||
			(parent == target.id && children[child] != string(v)) {
			return ErrDSGExactPreserve
		}
		return nil
	})
	if err != nil {
		return err
	}

	// A view of an existing native snapshot shares its parent directory.
	allocation := target.id
	if target.kind == snapshots.KindView && expectedParent != 0 {
		allocation = expectedParent
	}
	dir, err := dsgOpenOwned(fd, "snapshots", true)
	if err != nil {
		return ErrDSGExactPreserve
	}
	defer dir.Close()
	ordinal := strconv.FormatUint(allocation, 10)
	entry, err := dsgOpenOwned(int(dir.Fd()), ordinal, true)
	if err != nil {
		return ErrDSGExactPreserve
	}
	entry.Close()
	out.DirectoryPresent = true
	r.edge("native-snapshot-directory", keys.key, strconv.FormatUint(target.id, 10), ordinal)
	o.Snapshot = out
	return nil
}

func (r *dsgGraphReader) blob(o *DSGDedicatedGraph, workerFD int, blob string, size int64) error {
	if !dsgGraphSHA256(blob) || size < 0 {
		return ErrDSGExactPreserve
	}
	fd := workerFD
	for _, part := range []string{"content", "blobs", "sha256"} {
		dir, err := dsgOpenOwned(fd, part, true)
		if err != nil {
			return ErrDSGExactPreserve
		}
		defer dir.Close()
		fd = int(dir.Fd())
	}
	file, err := dsgOpenOwned(fd, strings.TrimPrefix(blob, "sha256:"), false)
	if err != nil {
		return ErrDSGExactPreserve
	}
	defer file.Close()
	info, err := file.Stat()
	// File payloads may contain arbitrary user data. Only compare metadata.
	if err != nil || info.Size() != size {
		return ErrDSGExactPreserve
	}
	o.Blob = &DSGGraphBlob{Size: size}
	r.edge("local-blob-allocation", blob, strconv.FormatInt(size, 10))
	return nil
}
