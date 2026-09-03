import React from 'react';
import { createRoot } from 'react-dom/client';
import { setWorkerUrl } from 'maplibre-gl';
import 'maplibre-gl/dist/maplibre-gl.css';
import maplibreWorkerUrl from 'maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url';
import App from './App';

// Bundlers can't resolve MapLibre's worker from `import.meta.url`; Vite's
// `?worker&url` emits a self-contained worker chunk we point it at instead.
setWorkerUrl(maplibreWorkerUrl);

const root = createRoot(document.getElementById('app')!);
root.render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
