//! Compare two prepared binaries section by section; prints the first mismatch.
fn load(p: &str) -> transit_data::PreparedData {
    let raw = std::fs::read(p).unwrap();
    transit_data::load(&transit_router::load_maybe_gzipped(&raw).unwrap()).unwrap()
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (x, y) = (load(&a[1]), load(&a[2]));
    macro_rules! chk {
        ($name:expr, $e:expr) => {
            if !$e {
                println!("DIFF: {}", $name);
            } else {
                println!("same: {}", $name);
            }
        };
    }
    chk!("num_nodes", x.num_nodes == y.num_nodes);
    let n = x.num_nodes.min(y.num_nodes);
    let first =
        (0..n).find(|&i| x.nodes[i].lat != y.nodes[i].lat || x.nodes[i].lon != y.nodes[i].lon);
    println!(
        "first node coord mismatch: {:?} (stops={} / {})",
        first, x.num_stops, y.num_stops
    );
    chk!(
        "stops names",
        x.stops
            .iter()
            .map(|s| &s.name)
            .eq(y.stops.iter().map(|s| &s.name))
    );
    chk!(
        "stop coords",
        x.stops
            .iter()
            .map(|s| (s.lat, s.lon))
            .eq(y.stops.iter().map(|s| (s.lat, s.lon)))
    );
    chk!("adj offsets", x.adj.offsets == y.adj.offsets);
    chk!("adj data", x.adj.data == y.adj.data);
    chk!("route names", x.route_names == y.route_names);
    chk!("pattern count", x.patterns.len() == y.patterns.len());
    for (i, (p, q)) in x.patterns.iter().zip(&y.patterns).enumerate() {
        let ev = p
            .events
            .iter()
            .map(|e| {
                (
                    e.time_offset,
                    e.stop_index,
                    e.travel_time,
                    e.next_event_index,
                )
            })
            .eq(q.events.iter().map(|e| {
                (
                    e.time_offset,
                    e.stop_index,
                    e.travel_time,
                    e.next_event_index,
                )
            }));
        let fr = p
            .frequency_routes
            .iter()
            .map(|f| {
                (
                    f.route_index,
                    f.stop_index,
                    f.start_time,
                    f.end_time,
                    f.headway_secs,
                    f.next_stop_index,
                    f.travel_time,
                    f.next_freq_index,
                )
            })
            .eq(q.frequency_routes.iter().map(|f| {
                (
                    f.route_index,
                    f.stop_index,
                    f.start_time,
                    f.end_time,
                    f.headway_secs,
                    f.next_stop_index,
                    f.travel_time,
                    f.next_freq_index,
                )
            }));
        if !ev || !fr || p.sentinel_routes != q.sentinel_routes {
            println!("DIFF: pattern {i} events={ev} freq={fr}");
            break;
        }
    }
    chk!(
        "stop_patterns",
        x.stop_patterns.offsets == y.stop_patterns.offsets
            && x.stop_patterns
                .data
                .iter()
                .map(|e| (
                    e.pattern,
                    e.event_start,
                    e.event_end,
                    e.freq_start,
                    e.freq_end
                ))
                .eq(y.stop_patterns.data.iter().map(|e| (
                    e.pattern,
                    e.event_start,
                    e.event_end,
                    e.freq_start,
                    e.freq_end
                )))
    );
    chk!("leg keys", x.leg_shape_keys == y.leg_shape_keys);
    chk!(
        "leg points",
        x.leg_shapes_lat == y.leg_shapes_lat && x.leg_shapes_lon == y.leg_shapes_lon
    );
}
