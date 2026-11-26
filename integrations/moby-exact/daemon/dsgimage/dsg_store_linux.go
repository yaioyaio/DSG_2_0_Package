//go:build linux

// Package dsgimage opens a sealed, archive-seeded Moby classic image store.
package dsgimage

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"

	"github.com/distribution/reference"
	_ "github.com/moby/moby/v2/daemon/graphdriver/vfs"
	"github.com/moby/moby/v2/daemon/internal/image"
	"github.com/moby/moby/v2/daemon/internal/layer"
	refstore "github.com/moby/moby/v2/daemon/internal/refstore"
	"github.com/opencontainers/go-digest"
	"github.com/opencontainers/image-spec/identity"
	"golang.org/x/sys/unix"
)

const (
	storeMaxImages = 32
	storeMaxLayers = 125
)

var errStore = errors.New("classic vfs image state is outside the sealed profile")

type imageBackend struct{ root string }

type storeLayer struct {
	chain, diff, parent, cache string
	size                       int64
}

type storeIndex struct {
	images []string
	layers []storeLayer
}

func storeHash(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, c := range value {
		if !(c >= 'a' && c <= 'f' || c >= '0' && c <= '9') {
			return false
		}
	}
	return true
}

func storeID(value string) bool {
	return strings.HasPrefix(value, "sha256:") && storeHash(value[7:])
}

// Never follow a symlink for metadata, directory roots, or VFS cache paths.
// The caller owns the entire private mount for the lifetime of the process.
func storeEntries(path string, limit int, optional bool) ([]os.DirEntry, error) {
	fi, err := os.Lstat(path)
	if optional && errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil || !fi.IsDir() {
		return nil, errStore
	}
	d, err := os.Open(path)
	if err != nil {
		return nil, errStore
	}
	defer d.Close()
	items, err := d.ReadDir(limit + 1)
	if err != nil && !errors.Is(err, io.EOF) || len(items) > limit {
		return nil, errStore
	}
	sort.Slice(items, func(i, j int) bool { return items[i].Name() < items[j].Name() })
	return items, nil
}

func storeRegular(path string, max int64) bool {
	fi, err := os.Lstat(path)
	return err == nil && fi.Mode().IsRegular() && fi.Size() <= max
}

func storeRead(path string, max int64) ([]byte, error) {
	if !storeRegular(path, max) {
		return nil, errStore
	}
	b, err := os.ReadFile(path)
	if err != nil || int64(len(b)) > max {
		return nil, errStore
	}
	return b, nil
}

func storeEmpty(path string, optional bool) bool {
	_, err := storeEntries(path, 0, optional)
	return err == nil
}

// Validate metadata before Moby restores it. Its restore functions can skip
// unreadable entries, and its layer loader recursively follows parent IDs.
func (b *imageBackend) index() (*storeIndex, error) {
	join := func(path string) string { return filepath.Join(b.root, path) }
	for _, path := range []string{"", "vfs", "image", "image/vfs", "image/vfs/imagedb", "image/vfs/imagedb/content", "image/vfs/imagedb/metadata", "image/vfs/layerdb"} {
		fi, err := os.Lstat(join(path))
		if err != nil || !fi.IsDir() {
			return nil, errStore
		}
	}
	for _, path := range []string{"image", "image/vfs/imagedb/content", "image/vfs/imagedb/metadata"} {
		entries, err := storeEntries(join(path), 1, false)
		expected := "sha256"
		if path == "image" {
			expected = "vfs"
		}
		if err != nil || len(entries) != 1 || entries[0].Name() != expected || !entries[0].IsDir() {
			return nil, errStore
		}
	}
	for _, path := range []string{"aufs", "btrfs", "devicemapper", "fuse-overlayfs", "overlay", "overlay2", "zfs"} {
		if _, err := os.Lstat(join(path)); !errors.Is(err, os.ErrNotExist) {
			return nil, errStore
		}
	}
	if !storeEmpty(join("containers"), false) || !storeEmpty(join("image/vfs/layerdb/mounts"), true) || !storeEmpty(join("image/vfs/layerdb/tmp"), true) {
		return nil, errStore
	}
	layerDB := join("image/vfs/layerdb")
	entries, err := storeEntries(layerDB, 3, false)
	if err != nil {
		return nil, err
	}
	for _, e := range entries {
		if !e.IsDir() || !slices.Contains([]string{"sha256", "mounts", "tmp"}, e.Name()) {
			return nil, errStore
		}
	}
	index := new(storeIndex)
	content := join("image/vfs/imagedb/content/sha256")
	contents, err := storeEntries(content, storeMaxImages, false)
	if err != nil {
		return nil, err
	}
	for _, e := range contents {
		if !storeHash(e.Name()) || !storeRegular(filepath.Join(content, e.Name()), 1<<20) {
			return nil, errStore
		}
		index.images = append(index.images, "sha256:"+e.Name())
	}
	metadataDir := join("image/vfs/imagedb/metadata/sha256")
	metadata, err := storeEntries(metadataDir, storeMaxImages, false)
	if err != nil {
		return nil, err
	}
	for _, e := range metadata {
		if !e.IsDir() || !slices.Contains(index.images, "sha256:"+e.Name()) {
			return nil, errStore
		}
		dir := filepath.Join(metadataDir, e.Name())
		files, err := storeEntries(dir, 3, false)
		if err != nil {
			return nil, err
		}
		for _, f := range files {
			path := filepath.Join(dir, f.Name())
			if !slices.Contains([]string{"parent", "lastUpdated"}, f.Name()) || !storeRegular(path, 256) {
				return nil, errStore
			}
			if f.Name() == "parent" {
				parent, err := storeRead(path, 71)
				if err != nil || !slices.Contains(index.images, string(parent)) {
					return nil, errStore
				}
			}
		}
	}
	layerEntries, err := storeEntries(filepath.Join(layerDB, "sha256"), storeMaxLayers, true)
	if err != nil {
		return nil, err
	}
	caches := make(map[string]bool)
	for _, e := range layerEntries {
		if !e.IsDir() || !storeHash(e.Name()) {
			return nil, errStore
		}
		dir := filepath.Join(layerDB, "sha256", e.Name())
		files, err := storeEntries(dir, 6, false)
		if err != nil {
			return nil, err
		}
		record := storeLayer{chain: "sha256:" + e.Name(), size: -1}
		for _, f := range files {
			path := filepath.Join(dir, f.Name())
			switch f.Name() {
			case "tar-split.json.gz":
				if !storeRegular(path, 16<<20) {
					return nil, errStore
				}
			case "descriptor.json":
				if !storeRegular(path, 8192) {
					return nil, errStore
				}
			case "size", "cache-id", "parent", "diff":
				data, err := storeRead(path, 71)
				if err != nil {
					return nil, err
				}
				value := string(data)
				switch f.Name() {
				case "size":
					size, err := strconv.ParseInt(value, 10, 64)
					if err != nil || size < 0 {
						return nil, errStore
					}
					record.size = size
				case "cache-id":
					record.cache = value
				case "parent":
					record.parent = value
					if !storeID(value) {
						return nil, errStore
					}
				case "diff":
					record.diff = value
				}
			default:
				return nil, errStore
			}
		}
		if record.size < 0 || !storeID(record.diff) || !storeHash(record.cache) || caches[record.cache] {
			return nil, errStore
		}
		expected := digest.Digest(record.diff)
		if record.parent != "" {
			expected = identity.ChainID([]digest.Digest{digest.Digest(record.parent), expected})
		}
		if string(expected) != record.chain {
			return nil, errStore
		}
		caches[record.cache] = true
		index.layers = append(index.layers, record)
	}
	for _, record := range index.layers {
		parent := record.parent
		for depth := 0; parent != ""; depth++ {
			found := slices.IndexFunc(index.layers, func(p storeLayer) bool { return p.chain == parent })
			if found == -1 || depth >= len(index.layers) {
				return nil, errStore
			}
			parent = index.layers[found].parent
		}
	}
	cacheDirs, err := storeEntries(join("vfs/dir"), storeMaxLayers, true)
	if err != nil || len(cacheDirs) != len(caches) {
		return nil, errStore
	}
	for _, e := range cacheDirs {
		if !e.IsDir() || !caches[e.Name()] {
			return nil, errStore
		}
	}
	return index, nil
}

func openImageStore(root string) (*imageBackend, error) {
	canonical, err := filepath.EvalSymlinks(root)
	if err != nil || canonical != root || !filepath.IsAbs(root) || root == "/" {
		return nil, errStore
	}
	// Receipt lookup remains available on the same sealed storage even when
	// an incomplete native operation prevents eligibility checks.
	return &imageBackend{root: root}, nil
}

type imageObservation struct {
	images   image.Store
	index    *storeIndex
	snapshot string
	estimate uint64
}

func (b *imageBackend) inspect(id, owner string) (*imageObservation, error) {
	if !storeID(id) || !storeHash(owner) {
		return nil, errStore
	}
	index, err := b.index()
	if err != nil || !slices.Contains(index.images, id) {
		return nil, errStore
	}
	location := filepath.Join(b.root, "image", "vfs")
	refPath := filepath.Join(location, "repositories.json")
	encoded, err := storeRead(refPath, 64<<10)
	if err != nil {
		return nil, err
	}
	// Native reload silently skips malformed references. Confirm the original
	// canonical representation and every reference before consulting its index.
	var onDisk struct{ Repositories map[string]map[string]string }
	if err := json.Unmarshal(encoded, &onDisk); err != nil || onDisk.Repositories == nil {
		return nil, errStore
	}
	original, err := json.Marshal(onDisk)
	if err != nil || !bytes.Equal(original, encoded) {
		return nil, errStore
	}
	for _, repository := range onDisk.Repositories {
		for name, refID := range repository {
			if _, err := reference.ParseNormalizedNamed(name); err != nil || !slices.Contains(index.images, refID) || refID == id {
				return nil, errStore
			}
		}
	}
	refs, err := refstore.NewReferenceStore(refPath)
	if err != nil || len(refs.References(digest.Digest(id))) != 0 {
		return nil, errStore
	}
	layerStore, err := layer.NewStoreFromOptions(layer.StoreOptions{Root: b.root, GraphDriver: "vfs"})
	if err != nil || layerStore.DriverName() != "vfs" {
		return nil, errStore
	}
	restored := layerStore.Map()
	if len(restored) != len(index.layers) {
		return nil, errStore
	}
	for _, record := range index.layers {
		if _, ok := restored[layer.ChainID(record.chain)]; !ok {
			return nil, errStore
		}
	}
	disk, err := image.NewFSStoreBackend(filepath.Join(location, "imagedb"))
	if err != nil {
		return nil, errStore
	}
	images, err := image.NewImageStore(disk, layerStore)
	if err != nil || images.Len() != len(index.images) {
		return nil, errStore
	}
	if len(images.Children(image.ID(id))) != 0 {
		return nil, errStore
	}
	digestState := sha256.New()
	fmt.Fprintf(digestState, "dsg-moby-classic-vfs-v2\x00%s\x00%s\x00%x\x00", id, owner, sha256.Sum256(encoded))
	// A key exists for every observed chain. Its value records whether another
	// image uses it, including through that image's base layers.
	usedByPeer := make(map[layer.ChainID]bool)
	for _, entry := range index.images {
		i, err := images.Get(image.ID(entry))
		if err != nil || i.OS != "linux" || i.Architecture != "arm64" || i.RootFS.Type != "layers" || len(i.RootFS.DiffIDs) > storeMaxLayers {
			return nil, errStore
		}
		if entry == id && (i.Config == nil || i.Config.Labels["io.dsg.gc.owner"] != owner) {
			return nil, errStore
		}
		chains := append([]digest.Digest(nil), i.RootFS.DiffIDs...)
		identity.ChainIDs(chains)
		for _, chain := range chains {
			if restored[chain] == nil {
				return nil, errStore
			}
			usedByPeer[chain] = usedByPeer[chain] || entry != id
		}
		fmt.Fprintf(digestState, "%s\x00%s\x00", entry, i.Parent)
	}
	if len(usedByPeer) != len(index.layers) {
		return nil, errStore
	}
	var estimate int64
	for _, record := range index.layers {
		shared := usedByPeer[layer.ChainID(record.chain)]
		fmt.Fprintf(digestState, "%s\x00%s\x00%d\x00%t\x00", record.chain, record.cache, record.size, shared)
		if !shared {
			if record.size > math.MaxInt64-estimate {
				return nil, errStore
			}
			estimate += record.size
		}
	}
	return &imageObservation{images: images, index: index, snapshot: hex.EncodeToString(digestState.Sum(nil)), estimate: uint64(estimate)}, nil
}

func (b *imageBackend) observe(id, owner string) (string, uint64, error) {
	observed, err := b.inspect(id, owner)
	if err != nil {
		return "", 0, err
	}
	return observed.snapshot, observed.estimate, nil
}

// The caller has durably consumed approval before invoking this exactly once.
// Moby may have changed disk even if Delete or a postcondition fails.
func (b *imageBackend) remove(id, snapshot, owner string, beforeDelete func() bool) error {
	observed, err := b.inspect(id, owner)
	if err != nil || observed.snapshot != snapshot || !beforeDelete() {
		return errStore
	}
	removed, err := observed.images.Delete(image.ID(id))
	if err != nil {
		return errStore
	}
	imagedb := filepath.Join(b.root, "image", "vfs", "imagedb")
	for _, dir := range []string{"content", "metadata"} {
		if _, err := os.Lstat(filepath.Join(imagedb, dir, "sha256", id[7:])); !errors.Is(err, os.ErrNotExist) {
			return errStore
		}
	}
	// Native deletion intentionally ignores some unlink errors. Verify that
	// layer database and VFS cache directories still match before issuing a receipt.
	after, err := b.index()
	if err != nil {
		return err
	}
	expectedImages := slices.DeleteFunc(observed.index.images, func(entry string) bool { return entry == id })
	if !slices.Equal(after.images, expectedImages) {
		return errStore
	}
	removedChains := make(map[string]bool)
	for _, item := range removed {
		removedChains[string(item.ChainID)] = true
	}
	expectedLayers := slices.DeleteFunc(observed.index.layers, func(record storeLayer) bool { return removedChains[record.chain] })
	if !slices.Equal(after.layers, expectedLayers) {
		return errStore
	}
	mount, err := os.Open(b.root)
	if err != nil {
		return errStore
	}
	defer mount.Close()
	if err := unix.Syncfs(int(mount.Fd())); err != nil {
		return errStore
	}
	return nil
}
