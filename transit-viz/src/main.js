import init, { TransitRouter } from '../pkg/transit_router.js';

// City definitions: add new cities here
const CITIES = [
  {
    id: 'chicago',
    name: 'Chicago, IL',
    file: 'chicago.bin',
    center: [41.88, -87.63],
    zoom: 12,
    detail: 'CTA buses & rail, Metra, Pace — 711K nodes',
  },
  {
    id: 'chapel_hill',
    name: 'Chapel Hill, NC',
    file: 'chapel_hill.bin',
    center: [35.913, -79.055],
    zoom: 14,
    detail: 'Chapel Hill Transit — small city test dataset',
  },
];

let router = null;
let map = null;
let sourceMarker = null;
let sourceNode = null;
let currentTravelTimes = null;
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

function updateSlackDisplay() {
  const val = parseInt(document.getElementById('slack-slider').value);
  const m = Math.floor(val / 60);
  const s = val % 60;
  document.getElementById('slack-display').textContent =
    `${m}:${String(s).padStart(2, '0')}`;
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
  const transferSlack = parseInt(document.getElementById('slack-slider').value);

  const status = document.getElementById('status');
  status.textContent = 'Computing...';

  setTimeout(() => {
    try {
      if (mode === 'single') {
        currentTravelTimes = router.run_tdd(sourceNode, depTime, patternIdx, transferSlack);
      } else {
        const nSamples = parseInt(document.getElementById('samples-slider').value);
        currentTravelTimes = router.run_tdd_sampled(
          sourceNode, depTime, depTime + 3600, nSamples, patternIdx, transferSlack
        );
      }
      renderIsochrone();
      const reached = currentTravelTimes.filter(t => !isNaN(t)).length;
      status.textContent = `Done. ${reached.toLocaleString()} nodes reached.`;
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

function populateCityList() {
  const list = document.getElementById('city-list');
  list.innerHTML = '';
  for (const city of CITIES) {
    const li = document.createElement('li');
    const btn = document.createElement('button');
    btn.className = 'city-btn';
    btn.innerHTML = `<div class="city-name">${city.name}</div><div class="city-detail">${city.detail}</div>`;
    btn.addEventListener('click', () => loadCity(city));
    li.appendChild(btn);
    list.appendChild(li);
  }
}

async function loadCity(city) {
  document.getElementById('city-select').style.display = 'none';
  const loadingOverlay = document.getElementById('loading-overlay');
  const loadingText = document.getElementById('loading-text');
  loadingOverlay.style.display = 'flex';
  loadingText.textContent = `Loading ${city.name}...`;

  // Reset state
  router = null;
  sourceMarker = null;
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
    await new Promise(r => setTimeout(r, 10)); // let UI update

    router = new TransitRouter(dataBytes);

    loadingOverlay.style.display = 'none';
    document.getElementById('controls').style.display = 'block';
    document.getElementById('legend').style.display = 'block';
    document.getElementById('city-title').textContent = city.name;
    document.getElementById('status').textContent =
      `${router.num_nodes().toLocaleString()} nodes, ${router.num_stops().toLocaleString()} stops. Click map to set origin.`;

    // Set up or recenter map
    if (!map) {
      initMap(city);
    } else {
      map.setView(city.center, city.zoom);
      if (ctx) ctx.clearRect(0, 0, canvas.width, canvas.height);
    }

    // Populate pattern selector
    const patternSelect = document.getElementById('pattern');
    patternSelect.innerHTML = '';
    const dayNames = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
    for (let i = 0; i < router.num_patterns(); i++) {
      const mask = router.pattern_day_mask(i);
      const days = dayNames.filter((_, j) => mask & (1 << j)).join(', ');
      const opt = document.createElement('option');
      opt.value = i;
      opt.textContent = `${days || 'Date-based'}`;
      patternSelect.appendChild(opt);
    }

    // Auto-select the pattern with the most coverage (most days set)
    let bestIdx = 0;
    let bestBits = 0;
    for (let i = 0; i < router.num_patterns(); i++) {
      const mask = router.pattern_day_mask(i);
      let bits = 0;
      for (let b = 0; b < 7; b++) if (mask & (1 << b)) bits++;
      if (bits > bestBits) { bestBits = bits; bestIdx = i; }
    }
    patternSelect.value = bestIdx;

  } catch (e) {
    loadingOverlay.style.display = 'none';
    document.getElementById('city-select').style.display = 'flex';
    alert(`Failed to load ${city.name}: ${e.message}`);
  }
}

function initMap(city) {
  map = L.map('map').setView(city.center, city.zoom);
  L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', {
    attribution: '&copy; OpenStreetMap contributors',
    maxZoom: 19,
  }).addTo(map);

  setupCanvas();

  // Re-render on map move
  map.on('moveend', renderIsochrone);
  map.on('zoomend', renderIsochrone);

  // Map click handler
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

  // Hover for travel time display
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

  document.getElementById('time-slider').addEventListener('input', updateTimeDisplay);
  document.getElementById('time-slider').addEventListener('change', runQuery);
  document.getElementById('samples-slider').addEventListener('input', updateSamplesDisplay);
  document.getElementById('samples-slider').addEventListener('change', runQuery);
  document.getElementById('pattern').addEventListener('change', runQuery);
  document.getElementById('slack-slider').addEventListener('input', updateSlackDisplay);
  document.getElementById('slack-slider').addEventListener('change', runQuery);

  // Change city button
  document.getElementById('change-city').addEventListener('click', () => {
    document.getElementById('controls').style.display = 'none';
    document.getElementById('legend').style.display = 'none';
    document.getElementById('hover-info').style.display = 'none';
    currentTravelTimes = null;
    if (ctx) ctx.clearRect(0, 0, canvas.width, canvas.height);
    if (sourceMarker) { sourceMarker.remove(); sourceMarker = null; }
    document.getElementById('city-select').style.display = 'flex';
  });
}

async function main() {
  await init();
  populateCityList();
}

main();
