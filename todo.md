Source: 51.495373, -0.091507
Destination: 51.537159, -0.001725

Mode: sampled
Date: 2026-04-12
Departure: 11:40
Samples: 30
Max time: 45 min
Transfer slack: 60s

Travel time: 35–37–40 min (31/31 samples)
Route:
  Walk 8 min
  SUTT-STAL · ELEPHANT & CASTLE Platform → FARRINGDON Platform 9 min
    Wait: 0.1 min
  Walk 0 min
  Elizabeth · FARRINGDON EL Platform → STRATFORD (LONDON) Platform 10 min
    Wait: 1.5 min
  Walk 8 min

wrong shape for northern line in london

-------

mixed straight line fallback and shapes for paris

-------

add busmaps api support

-------

since city bin build is embarrassingly parallel, can we run the build on a distributed cluster? the job is preemptible (just retry if preempted, takes at most ~2 minutes). is this easy to do for github actions?

-------

make use of the z order sorting of nodes to reduce memory footprint of node snapping index in frontend

-------

compress graph by merging adjacent walking nodes (with absolute + relative distance guarantee)?

-------

analytically figure out all optimal (no other trip with earlier arrival and later departure) arrival times and their departure times rather than sampling departure times?
- each node's state is a list of (arrival_time, departure_time, prev) pairs, sorted by arrival_time
- constrain departure_time to be within the sampling window
- both arrival_time and departure_time are strictly increasing (e.g. no earlier departure for a later arrival)
- start the search from the latest posssible departure time, and explore backwards in time
- special case for initial walking. first run a full walking pass to find all reachable nodes by walk. for every walk reachable stop, scan through all departures from that stop with walk departure time within the set window, sort them by departure time (using transit event time - initial walk time as departure time)
- in reverse departure time order, run normal dijkstra search on existing dijkstra state, but re-propagate when anything is relaxed (relaxed by adding an earlier arrival time to the front. note that departure time is always decreasing due to the order of scanning walk reachable stops)
- rinse and repeat
