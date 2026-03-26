import init, { TransitRouter } from '../pkg/transit_router.js';

let router = null;
let map = null;
let sourceMarker = null;
let sourceNode = null;
let currentTravelTimes = null;
let pathLayer = null;
let canvas, ctx;

// Color scale: green -> yellow -> orange -> red -> dark red
function travelTimeColor(seconds) {
  if (isNaN(seconds) || seconds < 0) return null;
  const maxTime = 7200; // 2 hours
  const t = Math.min(seconds / maxTime, 1.0);

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

function updateTimeDisplay() {
  const slider = document.getElementById('time-slider');
  document.getElementById('time-display').textContent = formatTime(parseInt(slider.value));
}

function updateSamplesDisplay() {
  const slider = document.getElementById('samples-slider');
  document.getElementById('samples-display').textContent = slider.value;
}

function renderIsochrone() {
  if (!router || !currentTravelTimes || !map || !canvas) return;

  const width = canvas.width;
  const height = canvas.height;
  ctx.clearRect(0, 0, width, height);

  const numNodes = router.num_nodes();
  const dotSize = Math.max(2, Math.min(6, 14 - map.getZoom()));

  for (let i = 0; i < numNodes; i++) {
    const tt = currentTravelTimes[i];
    if (isNaN(tt) || tt < 0) continue;

    const color = travelTimeColor(tt);
    if (!color) continue;

    const lat = router.node_lat(i);
    const lon = router.node_lon(i);
    const point = map.latLngToContainerPoint([lat, lon]);

    if (point.x < -dotSize || point.x > width + dotSize ||
        point.y < -dotSize || point.y > height + dotSize) continue;

    ctx.fillStyle = `rgba(${color[0]},${color[1]},${color[2]},0.6)`;
    ctx.fillRect(point.x - dotSize/2, point.y - dotSize/2, dotSize, dotSize);
  }
}

function runQuery() {
  if (!router || sourceNode === null) return;

  const mode = document.getElementById('mode').value;
  const patternIdx = parseInt(document.getElementById('pattern').value);
  const depTime = parseInt(document.getElementById('time-slider').value);

  const status = document.getElementById('status');
  status.textContent = 'Computing...';

  setTimeout(() => {
    try {
      if (mode === 'single') {
        currentTravelTimes = router.run_tdd(sourceNode, depTime, patternIdx);
      } else {
        const nSamples = parseInt(document.getElementById('samples-slider').value);
        currentTravelTimes = router.run_tdd_sampled(
          sourceNode, depTime, depTime + 3600, nSamples, patternIdx
        );
      }
      renderIsochrone();
      status.textContent = `Done. ${currentTravelTimes.filter(t => !isNaN(t)).length} nodes reached.`;
    } catch (e) {
      status.textContent = `Error: ${e}`;
      console.error(e);
    }
  }, 10);
}

function setupCanvas() {
  canvas = document.getElementById('overlay');
  ctx = canvas.getContext('2d');
  const resize = () => {
    canvas.width = window.innerWidth;
    canvas.height = window.innerHeight;
    renderIsochrone();
  };
  window.addEventListener('resize', resize);
  resize();
}

async function main() {
  // Initialize WASM
  await init();

  // Load binary data
  const status = document.getElementById('status');
  status.textContent = 'Loading transit data...';

  let dataBytes;
  try {
    const resp = await fetch('/data/city.bin');
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    dataBytes = new Uint8Array(await resp.arrayBuffer());
  } catch (e) {
    status.textContent = `Failed to load data: ${e.message}. Place city.bin in public/data/`;
    return;
  }

  status.textContent = 'Initializing router...';
  router = new TransitRouter(dataBytes);

  status.textContent = `Loaded: ${router.num_nodes()} nodes, ${router.num_stops()} stops. Click map to set origin.`;

  // Set up map centered on the data
  const centerLat = router.node_lat(0);
  const centerLon = router.node_lon(0);

  map = L.map('map').setView([centerLat, centerLon], 13);
  L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', {
    attribution: '&copy; OpenStreetMap contributors',
    maxZoom: 19,
  }).addTo(map);

  setupCanvas();

  // Re-render on map move
  map.on('moveend', renderIsochrone);
  map.on('zoomend', renderIsochrone);

  // Populate pattern selector
  const patternSelect = document.getElementById('pattern');
  const dayNames = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
  for (let i = 0; i < router.num_patterns(); i++) {
    const mask = router.pattern_day_mask(i);
    const days = dayNames.filter((_, j) => mask & (1 << j)).join(', ');
    const opt = document.createElement('option');
    opt.value = i;
    opt.textContent = `Pattern ${i}: ${days || 'Date-based'}`;
    patternSelect.appendChild(opt);
  }

  // Map click handler
  map.on('click', (e) => {
    const { lat, lng } = e.latlng;
    sourceNode = router.snap_to_node(lat, lng);
    const snapLat = router.node_lat(sourceNode);
    const snapLon = router.node_lon(sourceNode);

    if (sourceMarker) {
      sourceMarker.setLatLng([snapLat, snapLon]);
    } else {
      sourceMarker = L.marker([snapLat, snapLon], {
        title: 'Origin',
      }).addTo(map);
    }

    runQuery();
  });

  // Hover for path display
  map.on('mousemove', (e) => {
    if (!router || !currentTravelTimes) return;

    const { lat, lng } = e.latlng;
    const node = router.snap_to_node(lat, lng);
    const tt = currentTravelTimes[node];

    const hoverInfo = document.getElementById('hover-info');
    if (isNaN(tt) || tt < 0) {
      hoverInfo.style.display = 'none';
      return;
    }

    const minutes = Math.round(tt / 60);
    document.getElementById('hover-time').textContent = `Travel time: ${minutes} min`;

    // Check if this node is a transit stop
    let stopInfo = '';
    for (let s = 0; s < router.num_stops(); s++) {
      if (router.stop_node(s) === node) {
        stopInfo = router.stop_name(s);
        break;
      }
    }
    document.getElementById('hover-stop').textContent = stopInfo;
    hoverInfo.style.display = 'block';
  });

  // Control event handlers
  document.getElementById('mode').addEventListener('change', (e) => {
    document.getElementById('samples-group').style.display =
      e.target.value === 'sampled' ? 'block' : 'none';
    runQuery();
  });

  document.getElementById('time-slider').addEventListener('input', () => {
    updateTimeDisplay();
  });
  document.getElementById('time-slider').addEventListener('change', () => {
    runQuery();
  });

  document.getElementById('samples-slider').addEventListener('input', () => {
    updateSamplesDisplay();
  });
  document.getElementById('samples-slider').addEventListener('change', () => {
    runQuery();
  });

  document.getElementById('pattern').addEventListener('change', () => {
    runQuery();
  });
}

main();
