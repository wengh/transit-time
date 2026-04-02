import init, { initThreadPool, TransitRouter, __markRayonReady } from '../../pkg/transit_router.js';

let wasmReady = false;

export async function initWasm() {
  if (wasmReady) return;
  await init();
  try {
    await initThreadPool(navigator.hardwareConcurrency || 4);
    __markRayonReady();
  } catch (e) {
    console.warn('WASM thread pool unavailable, using single-threaded mode:', e);
  }
  wasmReady = true;
}

export async function loadRouter(cityFile, onProgress) {
  const resp = await fetch(`/data/${cityFile}`);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  const total = parseInt(resp.headers.get('content-length') || '0');
  let loaded = 0;

  const reader = resp.body.getReader();
  const chunks = [];
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    loaded += value.length;
    if (total > 0 && onProgress) {
      onProgress(Math.round(loaded / total * 100));
    }
  }
  const dataBytes = new Uint8Array(loaded);
  let offset = 0;
  for (const chunk of chunks) {
    dataBytes.set(chunk, offset);
    offset += chunk.length;
  }

  return new TransitRouter(dataBytes);
}

export function freeSsspList(ssspList) {
  if (!ssspList) return;
  for (const sssp of ssspList) {
    try { sssp.free(); } catch (_) {}
  }
}

export function runQuery(router, { sourceNode, mode, departureTime, date, nSamples, transferSlack, maxTime, prevSsspList }) {
  // Free previous results before allocating new ones to avoid WASM OOM
  freeSsspList(prevSsspList);
  const numNodes = router.num_nodes();

  if (mode === 'single') {
    const sssp = router.run_tdd_full_for_date(sourceNode, departureTime, date, transferSlack, maxTime);
    const ssspList = [sssp];
    const travelTimes = new Float64Array(numNodes);
    for (let i = 0; i < numNodes; i++) {
      const arr = router.node_arrival_time(sssp, i);
      travelTimes[i] = arr < 0xFFFFFFFF ? (arr - departureTime) : NaN;
    }
    return { travelTimes, ssspList };
  } else {
    const windowEnd = departureTime + 3600;
    const ssspList = router.run_tdd_sampled_full_for_date(
      sourceNode, departureTime, windowEnd, nSamples, date, transferSlack, maxTime
    );
    const sumTimes = new Float64Array(numNodes);
    const counts = new Uint32Array(numNodes);
    for (let s = 0; s < ssspList.length; s++) {
      const sssp = ssspList[s];
      const t = router.sssp_departure_time(sssp);
      for (let i = 0; i < numNodes; i++) {
        const arr = router.node_arrival_time(sssp, i);
        if (arr < 0xFFFFFFFF) {
          sumTimes[i] += (arr - t);
          counts[i]++;
        }
      }
    }
    const travelTimes = new Float64Array(numNodes);
    for (let i = 0; i < numNodes; i++) {
      travelTimes[i] = counts[i] > 0 ? sumTimes[i] / counts[i] : NaN;
    }
    return { travelTimes, ssspList };
  }
}

export function getHoverData(router, ssspList, node) {
  const allPaths = [];
  for (const sssp of ssspList) {
    if (sssp.__wbg_ptr === 0) continue; // Skip freed wasm objects
    try {
      const depTime = router.sssp_departure_time(sssp);
      const arrival = router.node_arrival_time(sssp, node);
      if (arrival >= 0xFFFFFFFF) {
        allPaths.push({ segments: [], totalTime: null });
        continue;
      }
      const pathArray = router.reconstruct_path(sssp, node);
      const segments = parsePathSegments(router, sssp, pathArray, depTime);
      allPaths.push({ segments, totalTime: arrival - depTime });
    } catch (e) {
      // In case of any Wasm errors about null pointers, skip safely
      if (e.message && e.message.includes('null pointer')) continue;
      throw e;
    }
  }
  return allPaths;
}

function parsePathSegments(router, sssp, pathArray, depTime) {
  const segments = [];
  let i = 0;
  while (i < pathArray.length) {
    const startIdx = i;
    const edgeType = pathArray[i + 1];
    const routeIdx = pathArray[i + 2];
    while (i + 3 < pathArray.length &&
           pathArray[i + 3 + 1] === edgeType &&
           pathArray[i + 3 + 2] === routeIdx) {
      i += 3;
    }
    const endIdx = i;
    const startNode = pathArray[startIdx];
    const endNode = pathArray[endIdx];
    const startTime = router.node_arrival_time(sssp, startNode);
    const endTime = router.node_arrival_time(sssp, endNode);

    const coords = [];
    for (let j = startIdx; j <= endIdx; j += 3) {
      const n = pathArray[j];
      coords.push([router.node_lat(n), router.node_lon(n)]);
    }

    let boardStopName = '';
    let boardNode = startNode;
    if (edgeType === 1 && segments.length > 0) {
      const prev = segments[segments.length - 1];
      boardStopName = prev.endStopName;
      boardNode = prev.endNodeIdx;
    }

    let finalCoords = coords;
    if (edgeType === 1 && routeIdx < 0xFFFFFFFF) {
      const shapeCoords = router.route_shape_between(routeIdx, boardNode, endNode);
      if (shapeCoords.length >= 4) {
        finalCoords = [];
        for (let k = 0; k < shapeCoords.length; k += 2) {
          finalCoords.push([shapeCoords[k], shapeCoords[k + 1]]);
        }
      } else if (segments.length > 0) {
        const prev = segments[segments.length - 1];
        if (prev.coords.length > 0) {
          coords.unshift(prev.coords[prev.coords.length - 1]);
        }
      }
    }

    let waitTime = 0;
    if (edgeType === 1) {
      const boardingTime = router.node_boarding_time(sssp, startNode);
      if (boardingTime > 0 && segments.length > 0) {
        const prev = segments[segments.length - 1];
        const arrivalAtBoardStop = router.node_arrival_time(sssp, prev.endNodeIdx);
        waitTime = boardingTime - arrivalAtBoardStop;
      } else if (boardingTime > 0) {
        const arrivalAtStop = router.node_arrival_time(sssp, boardNode);
        waitTime = boardingTime - arrivalAtStop;
      }
    }

    let duration;
    if (edgeType === 1 && waitTime >= 0) {
      const boardingTime = router.node_boarding_time(sssp, startNode);
      duration = boardingTime > 0 ? (endTime - boardingTime) : (endTime - startTime);
    } else {
      duration = endTime - startTime;
    }

    segments.push({
      edgeType,
      routeIdx,
      routeName: edgeType === 1 && routeIdx < 0xFFFFFFFF ? router.route_name(routeIdx) : '',
      startStopName: edgeType === 1 ? boardStopName : router.node_stop_name(startNode),
      endStopName: router.node_stop_name(endNode),
      endNodeIdx: endNode,
      duration,
      waitTime,
      coords: finalCoords,
    });
    i += 3;
  }
  return segments;
}
