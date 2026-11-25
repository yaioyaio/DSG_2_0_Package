//go:build linux

package cache

// DSGTransitiveCache summarizes the cache records that can reach the target
// or be reached from it by following parent links and equal-mutable aliases.
// Records includes the target. Ancestors and descendants exclude records that
// can already be reached from the target through aliases alone.
type DSGTransitiveCache struct {
	Records      int `json:"records"`
	Ancestors    int `json:"ancestors"`
	Descendants  int `json:"descendants"`
	AliasRecords int `json:"alias_records"`
	ParentLinks  int `json:"parent_links"`
	AliasLinks   int `json:"alias_links"`
}

type dsgCacheGraphLink struct {
	from, to, role string
}

type dsgCacheGraph struct {
	records map[string]struct{}
	links   []dsgCacheGraphLink
}

func (g *dsgCacheGraph) add(r *dsgGraphReader, id string, record dsgGraphRecord) error {
	links := len(record.parents)
	if record.alias != "" {
		links++
	}
	if !r.spend(links) {
		return ErrDSGExactPreserve
	}
	g.records[id] = struct{}{}
	for _, parent := range record.parents {
		g.links = append(g.links, dsgCacheGraphLink{from: id, to: parent.id, role: parent.role})
	}
	if record.alias != "" {
		g.links = append(g.links, dsgCacheGraphLink{from: id, to: record.alias, role: keyEqualMutable})
	}
	return nil
}

func (r *dsgGraphReader) cacheClosure(g *dsgCacheGraph, id string) *DSGTransitiveCache {
	paths := dsgNewPaths[string]()
	aliases := make(map[string][]string)
	loads := make(map[string][]string)
	incoming := make(map[string]int)
	for _, link := range g.links {
		if _, ok := g.records[link.to]; !ok {
			return nil
		}
		// BuildKit loads both parents and equalMutable by following the stored
		// direction. The reverse alias is a relationship for reference queries.
		loads[link.from] = append(loads[link.from], link.to)
		incoming[link.to]++
		paths.add(link.from, link.to)
		if link.role == keyEqualMutable {
			paths.add(link.to, link.from)
			aliases[link.from] = append(aliases[link.from], link.to)
			aliases[link.to] = append(aliases[link.to], link.from)
		}
	}
	if !dsgCacheGraphAcyclic(g.records, loads, incoming) {
		return nil
	}
	seeds := []string{id}
	ancestors := dsgReach(seeds, paths.forward)
	descendants := dsgReach(seeds, paths.reverse)
	equivalent := dsgReach(seeds, aliases)
	out := &DSGTransitiveCache{
		Ancestors:    len(ancestors) - len(equivalent),
		Descendants:  len(descendants) - len(equivalent),
		AliasRecords: len(equivalent) - 1,
	}
	for record := range g.records {
		if ancestors[record] || descendants[record] {
			out.Records++
		}
	}
	// The scan preserves Bolt's record order and BuildKit's merge parent order.
	for _, link := range g.links {
		if !(ancestors[link.from] && ancestors[link.to]) && !(descendants[link.from] && descendants[link.to]) {
			continue
		}
		if link.role == keyEqualMutable {
			out.AliasLinks++
		} else {
			out.ParentLinks++
		}
		r.edge("cache-transitive-link", link.role, link.from, link.to)
	}
	return out
}

func dsgCacheGraphAcyclic(records map[string]struct{}, loads map[string][]string, incoming map[string]int) bool {
	ready := make([]string, 0, len(records))
	for id := range records {
		if incoming[id] == 0 {
			ready = append(ready, id)
		}
	}
	for i := 0; i < len(ready); i++ {
		for _, next := range loads[ready[i]] {
			incoming[next]--
			if incoming[next] == 0 {
				ready = append(ready, next)
			}
		}
	}
	return len(ready) == len(records)
}
