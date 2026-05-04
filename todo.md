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

DONE

-------

compress graph by contracting clusters of adjacent walking nodes (e.g. intersections), with all distance $(d' - d) < \epsilon d + \delta$ for something like $\epsilon = 0.05, \delta = 10$?

remove 1-degree nodes with short edges and no transit stop (e.g. driveways)?

REJECTED

-------

analytically figure out all optimal (no other trip with earlier arrival and later departure) arrival times and their departure times rather than sampling departure times?
- each node's state is a list of (arrival_time, departure_time, prev) pairs, sorted by arrival_time
- constrain departure_time to be within the sampling window
- both arrival_time and departure_time are strictly increasing (e.g. no earlier departure for a later arrival)
- start the search from the latest posssible departure time, and explore backwards in time
- special case for initial walking. first run a full walking pass to find all reachable nodes by walk. for every walk reachable stop, scan through all departures from that stop with walk departure time within the set window, sort them by departure time (using transit event time - initial walk time as departure time)
- in reverse departure time order, run normal dijkstra search on existing dijkstra state, but re-propagate when anything is relaxed (relaxed by adding an earlier arrival time to the front. note that departure time is always decreasing due to the order of scanning walk reachable stops)
- rinse and repeat


can you explain why this is needed and what's wrong with our departure & arrival time tracking at each node? also make sure to formalize this into a routing test and assert that it has shape and initial time


could we store edge_dep_delta (which is vehicle_dep_delta if edge is transit, or arrival_delta - edge_weight if walk, or sentinel if is initial walk) instead of home_dep_delta? then for backtracking we can just binary search edge_dep_delta against arrival_delta of prev node's profile entry array (which works since it's sorted by descending arrival_delta), or the first value if is initial walk. i think that should let us get rid of tracking boarding time separately, and we can still recover the full path (thus getting departure from home time) so we don't lose any information. ultrathink about the tradeoffs (if any)


for your "what it costs":
1. that's just a binary search. almost trivial to get right.
2. the invariant is that we don't store any dominated entries so later arrival => later departure and FIFO property isn't a concern. that's the core invariant, not a subtle invariant
3. that's fine because we need to reconstruct the path to show the plot and the shapes anyway
4. same as 2, pareto optimal is the core invariant
5. addressing technical debt now is better than later. i can help revert your changes that add vehicle_dep, etc if you want.
what do you think?


also, for backtracking, we need to distinguish between switching to initial walk and backtracking to more transit. i think this can be resolved by keeping track (while backtracking only) of the best departure time in any visited node that have an initial walk entry (i.e. next_edge_dep_delta - initial_walk.arrival_delta) and the departure time's corresponding switch node (where we change from transit to initial walk). then after we exhaust backtracking (arriving at source or a node with only initial walk entry), just undo the backtracking to the node with best recorded initial walk departure time, and switch to backtrack the initial walk path from there. what do you think about this approach? is this a real problem that we need to solve? if so, did you already realize this problem? is there a simpler correct solution?
alternative: change the route id field to u16 and add a new bool flag for each transit entry to indicate whether its prev edge is initial walk or not.
ultrathink

change route_index to u16 and use a bool for the flag. this way we free up space for adding more bool flags later as well. when we start an initial transit leg (from initial walk), set the bool flag for this entry only if it's not there already (i.e. if a later departure reached here then the initial walk is non-optimal so shouldnt set the flag). is this correct? ultrathink

update the readme


Source: 41.883251, -87.627007
Destination: 41.815212, -87.689415

Mode: sampled
Date: 2026-04-16
Departure: 11:00
Samples: 15
Max time: 45 min
Transfer slack: 60s

Travel time: 30–33–44 min (9/9 samples)
Route:
  Walk 1 min
  Green Line · Washington/Wabash → Adams/Wabash 0 min
  Orange Line · Adams/Wabash → Western-Orange -3 min
  Walk 1 min
  49 · Western Orange Line Station → Western & 43rd Street 3 min
    Wait: 2.0 min
  Walk 5 min

ok this is still not working and getting unwieldy. can you make sure the profile routing is a well defined interface that has 2 functions:
1. run routing from source -> returns isochrone
2. get optimal paths to a specific destination
and that everything outside this interace work well
then i'll manually implement profile routing.
ultrathink

also as much as possible of the logic should live in the rust side to make it easy to test. for example shapes should be returned as rust objects and only converted to json at the wasm boundary

DONE

-------

when backtracking we need the route/trip id of the transit leg, and early termination once found the desired transit leg. is there a way to satisfy this need without compromising performance and without duplicating too much code?

i changed prep to ensure each node has at most 1 snapped stop, so we can store a dict of node_id -> stop_idx instead of a sparse jagged array. review my changes to prep and corresponding changes to router

DONE

-------

change shape dp to split segment at where it's closest to the stop location

handle stale schedules better? idk

profile routing performance
- get flamegraph of profile routing
- maybe add parallelism somewhere?
- maybe use a linked list of entries instead of an array of vectors?

DONE

-------

allow configurable departure window by changing the departure time slider to be 2 ended where dragging an endpoint changes the endpoint and dragging the middle changes both endpoints (keeping the same duration). limit the duration to 16 hours so it fits well in u16.

DONE

-------

use rayon (already installed) to parallelize router phase 3 and path reconstruction

DONE

-------

in many cities like chicago and paris, a single intersection can have as many as ~10-30 nodes. is there an easy way to identify these situations (either directly from the processed graph or maybe use some info from the osm pbf) and replace them with e.g. 4 nodes (or as many as the polygon shape needs)? ultrathink

try write a diagnostics script for option 3
also for your concerns:
- plaza should also be contracted
- we could do a convex hull then simplify it (remove corners with small angles, remove points that are close together)
- i don't think we're currently counting signal wait time at all? pls double check this
- to avoid interfering with stops, do this step before we snap stops (but for diagnostics just do it on the final graph)

UPDATE: this idea kinda preserves the distances but makes the walk edges not follow streets, often causing diagonal edges, etc.

-------

remove all degree 2 nodes

DONE

-------

how difficult would it be to identify nearly colinear cycles and contract them into a chain?

maybe do this: find all triangles in the graph where the longer side is more than k x (where k = 0.9 for example) the sum of the shorter sides (so losing the longer side doesn't hurt too much), and remove the longer side. this won't work for e.g. 4-cycles though. (this might be a very bad approach so use your own judgement)

make sure to do this before collapsing deg=2 nodes.

also before implementing anything, write a standalone diagnostic script to get stats on the osm pbf files for how common the situation appears

UPDATE: only results in ~1% saving

-------

read readme and profile.rs. then consider this:
split long departure window into several separate queries, compute in parallel, and merge results at the end. this should be done transparently behind profile routing interface as a separate implementation of the interface which uses the current implementation as a subroutine.

DONE

-------

from claude design output:

Transit Time – UI redesign changes to implement

Mobile UI (viewport < 640px)
Replace the existing bottom-sheet controls + floating HoverInfo with:
New: Top bar

Fixed to top, background: rgba(18,18,20,0.95) with backdrop-filter: blur(10px) and a subtle bottom border
Left: city name
Center: Origin / Dest segmented toggle — replaces long-press-to-set-origin. Tapping "Origin" then tapping the map sets the source. Tapping "Dest" then tapping the map pins the destination. After setting origin, auto-switches to Dest mode. Tapping again pins new destination.
Right: gear icon → opens settings sheet
Below the toggle: one-line contextual hint ("Tap map to set origin", "Tap map to set destination", "Computing…")
New: Bottom info strip + expandable drawer (replaces floating HoverInfo)

Collapsed: 56px tall. Shows summary line: avg travel time + reachability, or selected trip time + departure. Drag handle at top.
Tap to expand: slides up to ~68vh with smooth transition
Expanded: route segment list (walk times, transit legs with route color, wait times, stop names), then the sawtooth chart
Sawtooth chart uses 5:2 aspect ratio on mobile (wide and short) instead of square
Only shown when a destination is hovered or pinned
New: Settings bottom sheet (replaces collapsible controls panel)

Opens on gear tap over a dim backdrop; tap backdrop to dismiss
Contains all existing controls: map style, date, departure window, max travel time, transfer slack, service pattern count, Change City, Copy Info
Mobile interaction model
Origin mode tap → sets source, triggers computation, auto-switches to Dest mode
Dest mode tap → pins destination; tap again to pin new destination
Remove long-press to set origin

DONE

-------

add a reverse index for transit trips so we can remove `prev` from `Entry` to reduce memory usage.
to recover an edge, we try the following:
- find a neighbour with same home departure time and correct walk time => non-initial walk edge
- search for transit legs arriving at this node, finding a boarding node with a matching home departure time and arrival time <= boarding time - slack, or a initial walk time = current arrival time - current home departure time - transit time => transit leg
- find a neighbour with initial walk time = arrival time - home departure time - edge distance => initial walk edge

DONE

-------

split into up to 2-3x thread count chunks for profile routing parallelism to account for uneven distribution

-------

remove isochrone from chunk routing

DONE

-------

bugs:
- in mobile the ui elements are dark even when in light mode
- in mobile safari the browser's own ui is white when in dark mode
- plot is not responsive to theme changes

DONE

-------

show interactive base map and controls before data file finishes loading

-------

in desktop mode add a button next to hint button of the plot to expand the details panel to take the whole screen width
