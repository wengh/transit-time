import init, { initThreadPool, TransitRouter, __markRayonReady } from '../pkg/transit_router.js';

const cityModules = import.meta.glob('../../cities/*.json', { eager: true });
const CITIES = Object.values(cityModules)
  .map(mod => mod.default || mod)
  .sort((a, b) => a.name.localeCompare(b.name));

let router = null;
let map = null;
let sourceMarker = null;
let sourceNode = null;
let currentTravelTimes = null;
let currentSsspList = null; // Array of SsspResult objects for path reconstruction
let nodeCoords = null; // Float64Array: [lat0, lon0, lat1, lon1, ...] cached from WASM
let currentCity = null;
let maxTimeSec = 2700; // current max travel time in seconds

function travelTimeColor(seconds) {
  if (isNaN(seconds) || seconds < 0) return null;
  const t = Math.min(seconds / maxTimeSec, 1.0);

  let r, g, b;
  if (t < 0.25) {
    const s = t / 0.25;
    r = Math.round(s * 255);
    g = 255;
    b = 0;
  } else if (t < 0.5) {
    const s = (t - 0.25) / 0.25;
    r = 255;
    g = Math.round(255 * (1 - s * 0.47));
    b = 0;
  } else if (t < 0.75) {
    const s = (t - 0.5) / 0.25;
    r = 255;
    g = Math.round(136 * (1 - s));
    b = 0;
  } else {
    const s = (t - 0.75) / 0.25;
    r = Math.round(255 * (1 - s * 0.47));
    g = 0;
    b = 0;
  }
  return [r, g, b];
}

function formatTime(seconds) {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

function getSelectedDate() {
  const dateStr = document.getElementById('date-picker').value;
  if (!dateStr) return 20260406; // Monday default
  // Convert "YYYY-MM-DD" to YYYYMMDD integer
  return parseInt(dateStr.replace(/-/g, ''), 10);
}

function updateTimeDisplay() {
  document.getElementById('time-display').textContent =
    formatTime(parseInt(document.getElementById('time-slider').value));
}

function updateSamplesDisplay() {
  document.getElementById('samples-display').textContent =
    document.getElementById('samples-slider').value;
}

function updateSlackDisplay() {
  const val = parseInt(document.getElementById('slack-slider').value);
  const m = Math.floor(val / 60);
  const s = val % 60;
  document.getElementById('slack-display').textContent =
    `${m}:${String(s).padStart(2, '0')}`;
}

function updateMaxTimeDisplay() {
  const val = parseInt(document.getElementById('maxtime-slider').value);
  document.getElementById('maxtime-display').textContent = `${val} min`;
  maxTimeSec = val * 60;
  updateLegend();
}

function updateLegend() {
  const maxMin = maxTimeSec / 60;
  document.getElementById('legend-mid').textContent = `${Math.round(maxMin / 2)}`;
  document.getElementById('legend-max').textContent = `${maxMin} min`;

  // Update gradient to reflect current maxTime
  const gradient = document.getElementById('legend-gradient');
  gradient.style.background =
    'linear-gradient(to right, #00ff00, #ffff00, #ff8800, #ff0000, #880000)';
}

function updatePatternInfo() {
  if (!router) return;
  const date = getSelectedDate();
  const dateStr = document.getElementById('date-picker').value;
  const matchCount = router.num_patterns_for_date(date);
  document.getElementById('pattern-info').textContent =
    `${dateStr}: ${matchCount} service pattern${matchCount !== 1 ? 's' : ''} active`;
}

let isoOverlay = null;

// Reusable WebGL resources (created once, reused across renders)
let glCanvas = null;
let gl = null;
let glProgram = null;
let glPosBuffer = null;
let glColorBuffer = null;

function initWebGL() {
  glCanvas = document.createElement('canvas');
  gl = glCanvas.getContext('webgl', { alpha: true, premultipliedAlpha: false, antialias: false });
  if (!gl) return false;

  const vsrc = `
    attribute vec2 a_pos;
    attribute vec4 a_color;
    uniform float u_pointSize;
    varying vec4 v_color;
    void main() {
      gl_Position = vec4(a_pos, 0.0, 1.0);
      gl_PointSize = u_pointSize;
      v_color = a_color;
    }`;
  const fsrc = `
    precision mediump float;
    varying vec4 v_color;
    void main() {
      gl_FragColor = v_color;
    }`;

  function compile(type, src) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    return s;
  }
  glProgram = gl.createProgram();
  gl.attachShader(glProgram, compile(gl.VERTEX_SHADER, vsrc));
  gl.attachShader(glProgram, compile(gl.FRAGMENT_SHADER, fsrc));
  gl.linkProgram(glProgram);
  gl.useProgram(glProgram);

  glPosBuffer = gl.createBuffer();
  glColorBuffer = gl.createBuffer();
  return true;
}

function renderIsochrone() {
  if (!router || !currentTravelTimes || !map) return;

  const bounds = map.getBounds();
  const zoom = map.getZoom();

  const padLat = (bounds.getNorth() - bounds.getSouth()) * 0.5;
  const padLng = (bounds.getEast() - bounds.getWest()) * 0.5;
  const renderBounds = L.latLngBounds(
    [bounds.getSouth() - padLat, bounds.getWest() - padLng],
    [bounds.getNorth() + padLat, bounds.getEast() + padLng],
  );

  const topLeft = map.project(renderBounds.getNorthWest(), zoom);
  const bottomRight = map.project(renderBounds.getSouthEast(), zoom);
  const w = Math.ceil(bottomRight.x - topLeft.x);
  const h = Math.ceil(bottomRight.y - topLeft.y);
  if (w <= 0 || h <= 0) return;

  // Lazy-init WebGL
  if (!gl && !initWebGL()) return;

  glCanvas.width = w;
  glCanvas.height = h;
  gl.viewport(0, 0, w, h);
  gl.clearColor(0, 0, 0, 0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  // Precompute Mercator projection constants to avoid per-point map.project() calls
  const scale = 256 * Math.pow(2, zoom);

  const numNodes = router.num_nodes();
  // Minimum 1m diameter: at equator, 1px = 40075016 / (256 * 2^z) meters
  const metersPerPx = 40075016 / scale;
  const minPx = 5 / metersPerPx;
  const dotSize = Math.max(minPx, Math.max(2, Math.min(6, 14 - zoom)));
  const ox = topLeft.x;
  const oy = topLeft.y;
  const invW2 = 2 / w;
  const invH2 = 2 / h;

  // Build position and color arrays from cached nodeCoords
  const positions = new Float32Array(numNodes * 2);
  const colors = new Uint8Array(numNodes * 4);
  let count = 0;
  const coords = nodeCoords;
  const times = currentTravelTimes;
  const maxT = maxTimeSec;

  for (let i = 0; i < numNodes; i++) {
    const tt = times[i];
    if (!(tt >= 0 && tt <= maxT)) continue; // handles NaN too

    const color = travelTimeColor(tt);
    if (!color) continue;

    const ci2 = i * 2;
    const lat = coords[ci2];
    const lon = coords[ci2 + 1];

    // Inline Web Mercator projection (same math as Leaflet's map.project)
    const x = scale * (lon / 360 + 0.5) - ox;
    const y = scale * (0.5 - Math.log(Math.tan(Math.PI / 4 + lat * Math.PI / 360)) / (2 * Math.PI)) - oy;

    if (x < -dotSize || x > w + dotSize || y < -dotSize || y > h + dotSize) continue;

    const ci = count * 2;
    positions[ci] = x * invW2 - 1;
    positions[ci + 1] = 1 - y * invH2;

    const cc = count * 4;
    colors[cc] = color[0];
    colors[cc + 1] = color[1];
    colors[cc + 2] = color[2];
    colors[cc + 3] = 153; // 0.6 * 255

    count++;
  }

  if (count === 0) return;

  // Upload positions
  const posLoc = gl.getAttribLocation(glProgram, 'a_pos');
  gl.bindBuffer(gl.ARRAY_BUFFER, glPosBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, positions.subarray(0, count * 2), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  // Upload colors
  const colorLoc = gl.getAttribLocation(glProgram, 'a_color');
  gl.bindBuffer(gl.ARRAY_BUFFER, glColorBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, colors.subarray(0, count * 4), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(colorLoc);
  gl.vertexAttribPointer(colorLoc, 4, gl.UNSIGNED_BYTE, true, 0, 0);

  // Set point size and draw
  gl.uniform1f(gl.getUniformLocation(glProgram, 'u_pointSize'), dotSize);
  gl.drawArrays(gl.POINTS, 0, count);
  gl.finish();

  // Swap overlay
  const oldOverlay = isoOverlay;
  isoOverlay = L.imageOverlay(glCanvas.toDataURL(), renderBounds, {
    opacity: 1,
    interactive: false,
    zIndex: 500,
  }).addTo(map);
  if (oldOverlay) map.removeLayer(oldOverlay);
}

function runQuery() {
  if (!router || sourceNode === null) return;

  const mode = document.getElementById('mode').value;
  const depTime = parseInt(document.getElementById('time-slider').value);
  const transferSlack = parseInt(document.getElementById('slack-slider').value);
  const date = getSelectedDate();
  const maxTime = parseInt(document.getElementById('maxtime-slider').value) * 60;

  const status = document.getElementById('status');
  status.textContent = 'Computing...';

  setTimeout(() => {
    const start = performance.now();
    try {
      if (mode === 'single') {
        const sssp = router.run_tdd_full_for_date(
          sourceNode, depTime, date, transferSlack, maxTime
        );
        currentSsspList = [sssp];
        // Derive travel times from SSSP
        const numNodes = router.num_nodes();
        currentTravelTimes = new Float64Array(numNodes);
        for (let i = 0; i < numNodes; i++) {
          const arr = router.node_arrival_time(sssp, i);
          currentTravelTimes[i] = arr < 0xFFFFFFFF ? (arr - depTime) : NaN;
        }
      } else {
        const nSamples = parseInt(document.getElementById('samples-slider').value);
        const windowEnd = depTime + 3600;

        currentSsspList = router.run_tdd_sampled_full_for_date(
          sourceNode, depTime, windowEnd, nSamples, date, transferSlack, maxTime
        );

        const numNodes = router.num_nodes();
        const sumTimes = new Float64Array(numNodes);
        const counts = new Uint32Array(numNodes);

        for (let s = 0; s < currentSsspList.length; s++) {
          const sssp = currentSsspList[s];
          const t = router.sssp_departure_time(sssp);
          for (let i = 0; i < numNodes; i++) {
            const arr = router.node_arrival_time(sssp, i);
            if (arr < 0xFFFFFFFF) {
              sumTimes[i] += (arr - t);
              counts[i]++;
            }
          }
        }
        currentTravelTimes = new Float64Array(numNodes);
        for (let i = 0; i < numNodes; i++) {
          currentTravelTimes[i] = counts[i] > 0 ? sumTimes[i] / counts[i] : NaN;
        }
      }
      renderIsochrone();
      const end = performance.now();
      status.textContent = `Done. Spent ${Math.round(end - start)} ms.`;
    } catch (e) {
      status.textContent = `Error: ${e}`;
      console.error(e);
    }
  }, 10);
}

function setupCanvas() {
  // No-op: isochrone rendering now uses L.ImageOverlay
  // The old overlay canvas is no longer needed
}

function populateCityList() {
  const list = document.getElementById('city-list');
  list.innerHTML = '';
  for (const city of CITIES) {
    const li = document.createElement('li');
    const btn = document.createElement('button');
    btn.className = 'city-btn';
    btn.innerHTML = `<div class="city-name">${city.name}</div><div class="city-detail">${city.detail}</div>`;
    btn.addEventListener('click', () => {
      history.replaceState(null, '', `/${city.id}`);
      loadCity(city);
    });
    li.appendChild(btn);
    list.appendChild(li);
  }
}

function getCityFromUrl() {
  const path = window.location.pathname.replace(/^\//, '').replace(/\/$/, '');
  return CITIES.find(c => c.id === path) || null;
}

async function loadCity(city) {
  currentCity = city;
  document.getElementById('city-select').style.display = 'none';
  const loadingOverlay = document.getElementById('loading-overlay');
  const loadingText = document.getElementById('loading-text');
  loadingOverlay.style.display = 'flex';
  loadingText.textContent = `Loading ${city.name}...`;

  // Reset state
  router = null;
  sourceNode = null;
  currentTravelTimes = null;

  try {
    const resp = await fetch(`/data/${city.file}`);
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
      if (total > 0) {
        const pct = Math.round(loaded / total * 100);
        loadingText.textContent = `Loading ${city.name}... ${pct}%`;
      }
    }
    const dataBytes = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
      dataBytes.set(chunk, offset);
      offset += chunk.length;
    }

    loadingText.textContent = `Initializing router for ${city.name}...`;
    await new Promise(r => setTimeout(r, 10));

    router = new TransitRouter(dataBytes);
    nodeCoords = router.all_node_coords(); // cache once, avoid per-frame WASM calls

    loadingOverlay.style.display = 'none';
    document.getElementById('controls').style.display = 'block';
    document.getElementById('legend').style.display = 'block';
    document.getElementById('city-title').textContent = city.name;

    // Set date picker to today
    const today = new Date();
    document.getElementById('date-picker').value = today.toISOString().slice(0, 10);
    updatePatternInfo();
    updateMaxTimeDisplay();

    document.getElementById('status').textContent =
      `${router.num_nodes().toLocaleString()} nodes, ${router.num_stops().toLocaleString()} stops. Click map to set origin.`;

    if (!map) {
      initMap(city);
    } else {
      map.setView(city.center, city.zoom);
      if (sourceMarker) { sourceMarker.remove(); sourceMarker = null; }
      if (isoOverlay) { map.removeLayer(isoOverlay); isoOverlay = null; }
    }

  } catch (e) {
    loadingOverlay.style.display = 'none';
    document.getElementById('city-select').style.display = 'flex';
    history.replaceState(null, '', '/');
    alert(`Failed to load ${city.name}: ${e.message}`);
  }
}

function initMap(city) {
  map = L.map('map').setView(city.center, city.zoom);
  L.tileLayer('https://{s}.basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}{r}.png', {
    attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>',
    maxZoom: 20,
    subdomains: 'abcd',
    crossOrigin: true,
  }).addTo(map);

  setupCanvas();

  map.on('moveend', renderIsochrone);
  map.on('zoomend', renderIsochrone);

  map.on('click', (e) => {
    if (!router) return;
    const { lat, lng } = e.latlng;
    sourceNode = router.snap_to_node(lat, lng);
    const snapLat = router.node_lat(sourceNode);
    const snapLon = router.node_lon(sourceNode);

    if (sourceMarker) {
      sourceMarker.setLatLng([snapLat, snapLon]);
    } else {
      sourceMarker = L.marker([snapLat, snapLon], { title: 'Origin' }).addTo(map);
    }

    runQuery();
  });

  let routePolylines = [];
  let lastHoveredNode = null;

  function clearRouteOverlay() {
    routePolylines.forEach(p => p.remove());
    routePolylines = [];
  }

  function parsePathSegments(sssp, pathArray, depTime) {
    // pathArray is flat [node, edgeType, routeIdx, ...] with incoming-edge labels.
    // For transit segments, boarding stop = last node of previous walk segment.
    const segments = [];
    let i = 0;
    while (i < pathArray.length) {
      const startIdx = i;
      const edgeType = pathArray[i + 1];
      const routeIdx = pathArray[i + 2];
      // Group consecutive entries with same edge type and route
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
        // Boarding stop is the last node of the previous segment
        const prev = segments[segments.length - 1];
        boardStopName = prev.endStopName;
        boardNode = prev.endNodeIdx;
      }

      // For transit segments, try to use actual route geometry
      let finalCoords = coords;
      if (edgeType === 1 && routeIdx < 0xFFFFFFFF) {
        const shapeCoords = router.route_shape_between(routeIdx, boardNode, endNode);
        if (shapeCoords.length >= 4) {
          finalCoords = [];
          for (let k = 0; k < shapeCoords.length; k += 2) {
            finalCoords.push([shapeCoords[k], shapeCoords[k + 1]]);
          }
        } else if (segments.length > 0) {
          // Fallback: prepend boarding stop coord
          const prev = segments[segments.length - 1];
          if (prev.coords.length > 0) {
            coords.unshift(prev.coords[prev.coords.length - 1]);
          }
        }
      }

      // Compute wait time for transit segments
      let waitTime = 0;
      if (edgeType === 1) {
        const boardingTime = router.node_boarding_time(sssp, startNode);
        if (boardingTime > 0 && segments.length > 0) {
          // Transfer wait: boarding_time - arrival at boarding stop
          const prev = segments[segments.length - 1];
          const arrivalAtBoardStop = router.node_arrival_time(sssp, prev.endNodeIdx);
          waitTime = boardingTime - arrivalAtBoardStop;
        } else if (boardingTime > 0) {
          // Initial wait: boarding_time - arrival at first transit stop (walked there)
          const arrivalAtStop = router.node_arrival_time(sssp, boardNode);
          waitTime = boardingTime - arrivalAtStop;
        }
      }

      // For transit segments, duration = ride time (arrival - boarding_time)
      // since startNode == endNode (prev_node points to boarding_node).
      let duration;
      if (edgeType === 1 && waitTime >= 0) {
        const boardingTime = router.node_boarding_time(sssp, startNode);
        duration = boardingTime > 0 ? (endTime - boardingTime) : (endTime - startTime);
      } else {
        duration = endTime - startTime;
      }

      const seg = {
        edgeType, // 0=walk, 1=transit
        routeIdx,
        routeName: edgeType === 1 && routeIdx < 0xFFFFFFFF ? router.route_name(routeIdx) : '',
        startStopName: edgeType === 1 ? boardStopName : router.node_stop_name(startNode),
        endStopName: router.node_stop_name(endNode),
        endNodeIdx: endNode,
        duration,
        waitTime,
        coords: finalCoords,
      };
      segments.push(seg);
      i += 3;
    }
    return segments;
  }

  // Color palette for transit routes
  const ROUTE_COLORS = ['#e6194b','#3cb44b','#4363d8','#f58231','#911eb4','#42d4f4','#f032e6','#bfef45','#469990','#e6beff'];

  function drawRouteSegments(allPaths) {
    clearRouteOverlay();
    const routeColorMap = {};
    let colorIdx = 0;

    for (const { segments } of allPaths) {
      for (const seg of segments) {
        if (seg.coords.length < 2) continue;
        let color, dashArray, weight;
        if (seg.edgeType === 0) {
          color = '#888';
          dashArray = '6, 8';
          weight = 3;
        } else {
          if (!(seg.routeName in routeColorMap)) {
            routeColorMap[seg.routeName] = ROUTE_COLORS[colorIdx % ROUTE_COLORS.length];
            colorIdx++;
          }
          color = routeColorMap[seg.routeName];
          dashArray = null;
          weight = 4;
        }
        const line = L.polyline(seg.coords, {
          color, weight, opacity: 1,
          dashArray, interactive: false,
        }).addTo(map);
        routePolylines.push(line);
      }
    }
  }

  function buildHoverPanel(allPaths, node) {
    const hoverInfo = document.getElementById('hover-info');
    const isSampled = allPaths.length > 1;

    // Collect travel times from all paths
    const travelTimes = allPaths
      .map(p => p.totalTime)
      .filter(t => t !== null && isFinite(t))
      .sort((a, b) => a - b);

    if (travelTimes.length === 0) {
      hoverInfo.style.display = 'none';
      return;
    }

    let html = '';
    if (isSampled) {
      const avg = Math.round(travelTimes.reduce((a, b) => a + b, 0) / travelTimes.length / 60);
      const reachCount = travelTimes.length;
      html += `<div style="font-weight:600;margin-bottom:6px">Avg travel time: ${avg} min (${reachCount}/${allPaths.length} reachable, showing median route)</div>`;
    } else {
      const minutes = Math.round(travelTimes[0] / 60);
      html += `<div style="font-weight:600;margin-bottom:6px">Travel time: ${minutes} min</div>`;
    }

    // Show route segments from most common (or single) path
    // Use the path with median travel time
    const medianPath = allPaths.filter(p => p.totalTime !== null && isFinite(p.totalTime));
    if (medianPath.length > 0) {
      const mid = medianPath[Math.floor(medianPath.length / 2)];
      html += '<div style="border-top:1px solid #ddd;padding-top:6px;margin-top:2px">';
      for (let si = 0; si < mid.segments.length; si++) {
        const seg = mid.segments[si];
        const durMin = Math.round(seg.duration / 60);
        if (seg.edgeType === 0) {
          html += `<div style="font-size:12px;color:#666;padding:2px 0">Walk ${durMin} min</div>`;
        } else {
          // Show wait time before transit segment
          if (seg.waitTime > 0) {
            const waitMin = (seg.waitTime / 60).toFixed(1);
            const label = si <= 1 ? 'Initial wait' : 'Transfer wait';
            html += `<div style="font-size:11px;color:#999;padding:1px 0;font-style:italic">${label}: ${waitMin} min</div>`;
          }
          const fromTo = (seg.startStopName && seg.endStopName)
            ? ` · ${seg.startStopName} → ${seg.endStopName}` : '';
          html += `<div style="font-size:12px;padding:2px 0"><b>${seg.routeName || 'Transit'}</b>${fromTo}  ${durMin} min</div>`;
        }
      }
      html += '</div>';
    }

    // Time distribution plot for sampled mode
    if (isSampled && travelTimes.length >= 2) {
      const minT = travelTimes[0];
      const maxT = travelTimes[travelTimes.length - 1];
      const avgT = travelTimes.reduce((a, b) => a + b, 0) / travelTimes.length;
      const minMin = Math.round(minT / 60);
      const maxMin = Math.round(maxT / 60);
      const avgMin = Math.round(avgT / 60);
      const range = maxT - minT;

      html += '<div style="border-top:1px solid #ddd;padding-top:6px;margin-top:6px">';
      html += '<canvas id="time-dist" height="32" style="width:100%;height:32px;display:block"></canvas>';
      html += `<div style="display:flex;justify-content:space-between;font-size:10px;color:#888;margin-top:2px">`;
      html += `<span>min ${minMin}</span><span>avg ${avgMin}</span><span>max ${maxMin}</span></div>`;
      html += '</div>';

      hoverInfo.innerHTML = html;
      hoverInfo.style.display = 'block';

      // Draw distribution on canvas
      const distCanvas = document.getElementById('time-dist');
      if (distCanvas) {
        // Match canvas pixel size to its CSS layout size
        const rect = distCanvas.getBoundingClientRect();
        distCanvas.width = Math.round(rect.width);
        distCanvas.height = Math.round(rect.height);
        const dctx = distCanvas.getContext('2d');
        const w = distCanvas.width, h = distCanvas.height;
        dctx.clearRect(0, 0, w, h);
        const y = h / 2;
        const pad = 8;
        const plotW = w - 2 * pad;

        // Horizontal line
        dctx.strokeStyle = '#ccc';
        dctx.lineWidth = 1;
        dctx.beginPath();
        dctx.moveTo(pad, y);
        dctx.lineTo(w - pad, y);
        dctx.stroke();

        // End ticks
        dctx.strokeStyle = '#aaa';
        dctx.beginPath();
        dctx.moveTo(pad, y - 6); dctx.lineTo(pad, y + 6);
        dctx.moveTo(w - pad, y - 6); dctx.lineTo(w - pad, y + 6);
        dctx.stroke();

        // Sample dots with y jitter to distinguish clusters
        dctx.fillStyle = '#4a90d9';
        for (let si = 0; si < travelTimes.length; si++) {
          const t = travelTimes[si];
          const x = range > 0 ? pad + ((t - minT) / range) * plotW : w / 2;
          // Deterministic jitter based on index to avoid overlap
          const jitter = ((si * 7 + 3) % 11 - 5) * 1.2;
          dctx.beginPath();
          dctx.arc(x, y + jitter, 3, 0, Math.PI * 2);
          dctx.fill();
        }

        // Average marker (triangle)
        const avgX = range > 0 ? pad + ((avgT - minT) / range) * plotW : w / 2;
        dctx.fillStyle = '#333';
        dctx.beginPath();
        dctx.moveTo(avgX, y - 8);
        dctx.lineTo(avgX - 4, y - 14);
        dctx.lineTo(avgX + 4, y - 14);
        dctx.closePath();
        dctx.fill();
      }
    } else {
      hoverInfo.innerHTML = html;
      hoverInfo.style.display = 'block';
    }
  }

  map.on('mousemove', (e) => {
    if (!router || !currentTravelTimes || !currentSsspList) return;

    const { lat, lng } = e.latlng;
    const node = router.snap_to_node(lat, lng);
    const tt = currentTravelTimes[node];

    const hoverInfo = document.getElementById('hover-info');
    if (isNaN(tt) || tt < 0) {
      hoverInfo.style.display = 'none';
      clearRouteOverlay();
      lastHoveredNode = null;
      return;
    }

    if (node === lastHoveredNode) return;
    lastHoveredNode = node;

    // Reconstruct paths from all SSSP results
    const allPaths = [];
    for (const sssp of currentSsspList) {
      const depTime = router.sssp_departure_time(sssp);
      const arrival = router.node_arrival_time(sssp, node);
      if (arrival >= 0xFFFFFFFF) {
        allPaths.push({ segments: [], totalTime: null });
        continue;
      }
      const pathArray = router.reconstruct_path(sssp, node);
      const segments = parsePathSegments(sssp, pathArray, depTime);
      allPaths.push({ segments, totalTime: arrival - depTime });
    }

    drawRouteSegments(allPaths.filter(p => p.segments.length > 0));
    buildHoverPanel(allPaths, node);
  });

  map.on('mouseout', () => {
    clearRouteOverlay();
    document.getElementById('hover-info').style.display = 'none';
    lastHoveredNode = null;
  });

  // Control event handlers
  document.getElementById('mode').addEventListener('change', (e) => {
    document.getElementById('samples-group').style.display =
      e.target.value === 'sampled' ? 'block' : 'none';
    runQuery();
  });

  document.getElementById('date-picker').addEventListener('change', () => {
    updatePatternInfo();
    runQuery();
  });
  document.getElementById('time-slider').addEventListener('input', updateTimeDisplay);
  document.getElementById('time-slider').addEventListener('change', runQuery);
  document.getElementById('samples-slider').addEventListener('input', updateSamplesDisplay);
  document.getElementById('samples-slider').addEventListener('change', runQuery);
  document.getElementById('maxtime-slider').addEventListener('input', () => {
    updateMaxTimeDisplay();
    renderIsochrone(); // re-render with new color scale immediately
  });
  document.getElementById('maxtime-slider').addEventListener('change', runQuery);
  document.getElementById('slack-slider').addEventListener('input', updateSlackDisplay);
  document.getElementById('slack-slider').addEventListener('change', runQuery);

  document.addEventListener('keydown', (e) => {
    if (e.key !== 'c' || e.ctrlKey || e.metaKey || e.altKey) return;
    // Don't fire if typing in an input
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'SELECT' || e.target.tagName === 'TEXTAREA') return;
    if (!router || sourceNode === null) return;

    const depTime = parseInt(document.getElementById('time-slider').value);
    const slack = parseInt(document.getElementById('slack-slider').value);
    const dateStr = document.getElementById('date-picker').value || 'N/A';

    const srcLat = router.node_lat(sourceNode).toFixed(6);
    const srcLon = router.node_lon(sourceNode).toFixed(6);

    let lines = [];
    lines.push(`Source: ${srcLat}, ${srcLon}`);

    // If hovering a destination, include it + path
    if (lastHoveredNode !== null && currentSsspList && currentSsspList.length > 0) {
      const destLat = router.node_lat(lastHoveredNode).toFixed(6);
      const destLon = router.node_lon(lastHoveredNode).toFixed(6);
      lines.push(`Destination: ${destLat}, ${destLon}`);
    }

    lines.push(`Date: ${dateStr}`);
    lines.push(`Departure: ${formatTime(depTime)}`);
    lines.push(`Transfer slack: ${slack}s`);

    if (lastHoveredNode !== null && currentSsspList && currentSsspList.length > 0) {
      const sssp = currentSsspList[0];
      const arrival = router.node_arrival_time(sssp, lastHoveredNode);
      if (arrival < 0xFFFFFFFF) {
        const dep = router.sssp_departure_time(sssp);
        const tt = arrival - dep;
        lines.push(`Travel time: ${Math.round(tt / 60)} min`);
        lines.push('');
        lines.push('Path:');

        const pathArray = router.reconstruct_path(sssp, lastHoveredNode);
        const segments = parsePathSegments(sssp, pathArray, dep);
        for (const seg of segments) {
          const durMin = Math.round(seg.duration / 60);
          if (seg.edgeType === 0) {
            lines.push(`  Walk ${durMin} min`);
          } else {
            const fromTo = (seg.startStopName && seg.endStopName)
              ? ` ${seg.startStopName} → ${seg.endStopName}` : '';
            lines.push(`  ${seg.routeName || 'Transit'}${fromTo} ${durMin} min`);
          }
        }
      }
    }

    const text = lines.join('\n');
    navigator.clipboard.writeText(text).then(() => {
      const status = document.getElementById('status');
      const prev = status.textContent;
      status.textContent = 'Copied to clipboard!';
      setTimeout(() => { status.textContent = prev; }, 1500);
    });
  });

  document.getElementById('change-city').addEventListener('click', () => {
    document.getElementById('controls').style.display = 'none';
    document.getElementById('legend').style.display = 'none';
    document.getElementById('hover-info').style.display = 'none';
    currentTravelTimes = null;
    if (isoOverlay) { map.removeLayer(isoOverlay); isoOverlay = null; }
    if (sourceMarker) { sourceMarker.remove(); sourceMarker = null; }
    history.replaceState(null, '', '/');
    document.getElementById('city-select').style.display = 'flex';
  });
}

async function main() {
  await init();
  try {
    await initThreadPool(navigator.hardwareConcurrency || 4);
    __markRayonReady();
  } catch (e) {
    console.warn('WASM thread pool unavailable, using single-threaded mode:', e);
  }
  populateCityList();

  // Check URL for direct city link
  const urlCity = getCityFromUrl();
  if (urlCity) {
    loadCity(urlCity);
  }
}

main();
