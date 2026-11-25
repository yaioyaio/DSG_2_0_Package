//go:build linux

package cache

import (
	"bytes"
	"slices"
	"strings"

	bolt "go.etcd.io/bbolt"
	"google.golang.org/protobuf/encoding/protowire"
)

const dsgExternalPayloadLimit = 2 << 20

type dsgHistoryDescriptor struct {
	role, blob string
}

func (r *dsgGraphReader) externalHistory(o *DSGGraphExternal, tx *bolt.Tx, target string) error {
	records, opaque, err := r.scanHistoryDescriptors(tx, func(refID, role, blob string) error {
		o.HistoryDescriptors++
		if blob == target {
			o.HistoryDescriptorReferences++
			r.edge("external-history-descriptor", refID, role, blob)
		}
		return nil
	})
	o.HistoryRecords, o.OpaqueHistoryRecords = records, opaque
	return err
}

// eachHistoryDescriptor supplies namespace buildkit_history roots for the
// transitive graph. Build references and names from the history DB stay private.
func (r *dsgGraphReader) eachHistoryDescriptor(tx *bolt.Tx, visit func(refID, role, blob string) error) (int, error) {
	records, opaque, err := r.scanHistoryDescriptors(tx, visit)
	if opaque != 0 {
		return records, ErrDSGExactPreserve
	}
	return records, err
}

func (r *dsgGraphReader) scanHistoryDescriptors(tx *bolt.Tx, visit func(refID, role, blob string) error) (records, opaque int, err error) {
	if tx == nil || tx.Writable() {
		return 0, 0, ErrDSGExactPreserve
	}
	if err := tx.ForEach(func(k []byte, _ *bolt.Bucket) error {
		switch string(k) {
		case "_version", "_records":
			return nil
		default:
			return ErrDSGExactPreserve
		}
	}); err != nil {
		return 0, 0, err
	}
	version := tx.Bucket([]byte("_version"))
	if version == nil || !bytes.Equal(version.Get([]byte("version")), []byte("2")) {
		return 0, 0, ErrDSGExactPreserve
	}
	if err := r.each(version, func(k, v []byte) error {
		if string(k) != "version" || v == nil {
			return ErrDSGExactPreserve
		}
		return nil
	}); err != nil {
		return 0, 0, err
	}
	err = r.each(tx.Bucket([]byte("_records")), func(key, raw []byte) error {
		records++
		if len(raw) == 0 || len(raw) > dsgExternalPayloadLimit || len(key) > 256 {
			opaque++
			return nil
		}
		descriptors, err := r.historyRecordDescriptors(key, raw)
		if err != nil {
			opaque++
			return nil
		}
		refID := dsgExternalIdentifier(key)
		for _, desc := range descriptors {
			if err := visit(refID, desc.role, desc.blob); err != nil {
				return err
			}
		}
		return nil
	})
	return records, opaque, err
}

// Read only the descriptor-bearing wire fields of pinned control.proto. Skip
// string maps, exporter configuration, error and annotations as whole fields.
func (r *dsgGraphReader) historyRecordDescriptors(key, raw []byte) ([]dsgHistoryDescriptor, error) {
	var descriptors []dsgHistoryDescriptor
	var seen uint32
	err := r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		if n > 19 || (n >= 12 && n != 13 && n != 18 && typ != protowire.VarintType) ||
			((n <= 11 || n == 13 || n == 18) && typ != protowire.BytesType) {
			return ErrDSGExactPreserve
		}
		if n != 3 && n != 4 && n != 9 && n != 11 {
			bit := uint32(1) << n
			if seen&bit != 0 {
				return ErrDSGExactPreserve
			}
			seen |= bit
		}
		switch n {
		case 1:
			if !bytes.Equal(key, v) {
				return ErrDSGExactPreserve
			}
		case 8:
			return r.historyDescriptor(v, "logs", &descriptors)
		case 10:
			return r.historyResult(v, "result", &descriptors)
		case 11:
			result, err := r.historyMapValue(v, protowire.BytesType)
			if err != nil {
				return err
			}
			return r.historyResult(result, "named-result", &descriptors)
		case 13:
			return r.historyDescriptor(v, "trace", &descriptors)
		case 18:
			return r.historyDescriptor(v, "external-error", &descriptors)
		}
		return nil
	})
	if err != nil || seen&(1<<1) == 0 {
		return nil, ErrDSGExactPreserve
	}
	// Protobuf map order is not stable between equivalent writes.
	slices.SortFunc(descriptors, func(a, b dsgHistoryDescriptor) int {
		if a.role != b.role {
			return strings.Compare(a.role, b.role)
		}
		return strings.Compare(a.blob, b.blob)
	})
	return descriptors, nil
}

func (r *dsgGraphReader) historyResult(raw []byte, role string, descriptors *[]dsgHistoryDescriptor) error {
	var deprecated bool
	return r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		if typ != protowire.BytesType {
			return ErrDSGExactPreserve
		}
		switch n {
		case 1:
			if deprecated {
				return ErrDSGExactPreserve
			}
			deprecated = true
			return r.historyDescriptor(v, role+"-deprecated", descriptors)
		case 2:
			return r.historyDescriptor(v, role+"-attestation", descriptors)
		case 3:
			desc, err := r.historyMapValue(v, protowire.VarintType)
			if err != nil {
				return err
			}
			return r.historyDescriptor(desc, role+"-output", descriptors)
		default:
			return ErrDSGExactPreserve
		}
	})
}

func (r *dsgGraphReader) historyMapValue(raw []byte, keyType protowire.Type) ([]byte, error) {
	var value []byte
	var seenKey bool
	err := r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		switch {
		case n == 1 && typ == keyType && !seenKey:
			seenKey = true
		case n == 2 && typ == protowire.BytesType && value == nil:
			value = v
		default:
			return ErrDSGExactPreserve
		}
		return nil
	})
	if err != nil || !seenKey || value == nil {
		return nil, ErrDSGExactPreserve
	}
	return value, nil
}

func (r *dsgGraphReader) historyDescriptor(raw []byte, role string, descriptors *[]dsgHistoryDescriptor) error {
	var blob string
	var seen uint8
	err := r.externalWire(raw, func(n protowire.Number, typ protowire.Type, v []byte) error {
		if n < 1 || n > 5 || n == 4 || (n == 3 && typ != protowire.VarintType) || (n != 3 && typ != protowire.BytesType) {
			return ErrDSGExactPreserve
		}
		if n != 5 {
			bit := uint8(1) << n
			if seen&bit != 0 {
				return ErrDSGExactPreserve
			}
			seen |= bit
		}
		if n == 2 {
			if len(v) != 71 || !dsgGraphSHA256(string(v)) {
				return ErrDSGExactPreserve
			}
			blob = string(v)
		}
		return nil
	})
	if err != nil || blob == "" {
		return ErrDSGExactPreserve
	}
	*descriptors = append(*descriptors, dsgHistoryDescriptor{role, blob})
	return nil
}

func (r *dsgGraphReader) externalWire(raw []byte, visit func(protowire.Number, protowire.Type, []byte) error) error {
	for len(raw) != 0 {
		r.remaining--
		if r.remaining < 0 || r.ctx.Err() != nil {
			return ErrDSGExactPreserve
		}
		n, typ, tagSize := protowire.ConsumeTag(raw)
		if tagSize < 0 || !n.IsValid() {
			return ErrDSGExactPreserve
		}
		raw = raw[tagSize:]
		var value []byte
		var size int
		switch typ {
		case protowire.BytesType:
			value, size = protowire.ConsumeBytes(raw)
		case protowire.VarintType:
			_, size = protowire.ConsumeVarint(raw)
		default:
			return ErrDSGExactPreserve
		}
		if size < 0 {
			return ErrDSGExactPreserve
		}
		if typ == protowire.VarintType {
			value = raw[:size]
		}
		if err := visit(n, typ, value); err != nil {
			return err
		}
		raw = raw[size:]
	}
	return nil
}
