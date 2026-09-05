import React, { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import { Map as MapLibreMap, Marker, type MapMouseEvent } from 'maplibre-gl';
import { useAppState } from '../state/AppContext';
import { animationStore, useAnimMode, useAnimRenderedDeparture } from '../state/animationStore';
import { cancelInflightQuery, snapToNode, type HoverPath } from '../utils/router';
import type { HoverData } from '../state/reducer';
import { deriveDisplayPath } from './HoverInfo';
import { ROUTE_COLORS } from '../utils/colors';
import { getHashParams, setHashParams } from '../utils/urlHash';
import { buildHoverData } from '../utils/hoverInfo';
import { resolveMapStyle, tuneStyleForZoomOut, REPO_ATTR, type MapStyle } from '../utils/mapStyles';
import { MapOverlays, toLngLat, type PointFeature, type RouteFeature } from '../utils/mapOverlays';
import { mapToSlippyZoom, slippyToMapZoom } from '../utils/zoom';
import { useIsMobile } from '../utils/useIsMobile';

export interface MapViewHandle {
  // Returns true if SET_SOURCE was dispatched (snap succeeded and the query
  // is about to run); false if the call queued the click instead, or bailed
  // because snapToNode returned null. The pending-source consumption effect
  // uses this to know whether to dispatch a fallback CONSUME_PENDING_SOURCE.
  setSource(lat: number, lng: number, opts?: { keepDest?: boolean }): Promise<boolean>;
  setDestination(lat: number, lng: number): Promise<void>;
  flyTo(lat: number, lng: number): void;
  zoomIn(): void;
  zoomOut(): void;
}

/** DOM pin for the origin. `opacity` < 1 marks a provisional (queued) origin. */
function makeOriginMarker(latLng: [number, number], title: string, opacity = 1): Marker {
  const marker = new Marker({ opacity: String(opacity) }).setLngLat(toLngLat(latLng));
  const el = marker.getElement();
  el.title = title;
  // Map clicks/hovers go to the canvas underneath; the pin is purely visual.
  el.style.pointerEvents = 'none';
  return marker;
}

const MapView = forwardRef<MapViewHandle>(function MapView(_props, ref): React.ReactNode {
  const { state, dispatch } = useAppState();
  const mapRef = useRef<MapLibreMap | null>(null);
  const mapContainerRef = useRef<HTMLDivElement>(null);
  const overlaysRef = useRef<MapOverlays | null>(null);
  const sourceMarkerRef = useRef<Marker | null>(null);
  // Faint marker placed at the click location while the city data is still
  // loading. Replaced/moved on each new pending click; removed once the real
  // (snapped) source marker takes its place.
  const provisionalSourceRef = useRef<Marker | null>(null);
  const appliedStyleRef = useRef<MapStyle | null>(null);
  const drawRouteLayersRef = useRef<((paths: HoverPath[]) => void) | null>(null);
  // Picks which route paths to draw for a destination — the full Pareto fan
  // (average view) or just the path optimal for the current departure time
  // (playback). Reassigned every render so imperative hover code reads the
  // live animation state without re-running the map-events effect.
  const resolveRoutePathsRef = useRef<(hd: HoverData) => HoverPath[]>(() => []);
  const lastHoveredNodeRef = useRef<number | null>(null);

  // Refs to closures (updated each time the map-events effect runs)
  // so the imperative handle can call them from outside MapView.
  const setSourceRef = useRef<
    ((lat: number, lng: number, opts?: { keepDest?: boolean }) => Promise<boolean>) | null
  >(null);
  const setDestinationRef = useRef<((lat: number, lng: number) => Promise<void>) | null>(null);

  useImperativeHandle(ref, () => ({
    setSource: (lat, lng, opts) => setSourceRef.current?.(lat, lng, opts) ?? Promise.resolve(false),
    setDestination: (lat, lng) => setDestinationRef.current?.(lat, lng) ?? Promise.resolve(),
    flyTo: (lat, lng) => {
      const map = mapRef.current;
      if (!map) return;
      map.flyTo({ center: [lng, lat], zoom: Math.max(map.getZoom(), slippyToMapZoom(14)) });
    },
    zoomIn: () => mapRef.current?.zoomIn(),
    zoomOut: () => mapRef.current?.zoomOut(),
  }));

  const stateRef = useRef(state);
  stateRef.current = state;

  // Width-based mobile detection. Mirrored to a ref so the once-installed
  // map click handlers can read the live value without being re-registered
  // when the viewport crosses the breakpoint.
  const isMobile = useIsMobile();
  const isMobileRef = useRef(isMobile);
  isMobileRef.current = isMobile;

  // Keep double-click-zoom in sync when the user crosses the breakpoint at
  // runtime (e.g. rotating a tablet, resizing a window).
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    if (isMobile) map.doubleClickZoom.enable();
    else map.doubleClickZoom.disable();
  }, [isMobile]);

  // Initialize map
  useEffect(() => {
    if (mapRef.current || !mapContainerRef.current) return;
    // Start on the style the URL hash restored (if any) rather than the
    // default, so the first style load is the only one.
    const initialStyle = resolveMapStyle(stateRef.current.mapStyle);
    const map = new MapLibreMap({
      container: mapContainerRef.current,
      center: [-90, 40],
      zoom: slippyToMapZoom(4),
      maxZoom: slippyToMapZoom(20),
      // Desktop uses double-click to set the source, so the default
      // double-click-to-zoom would conflict. On mobile that gesture is unused,
      // so keep the default zoom behavior there.
      doubleClickZoom: isMobileRef.current,
      // North-up, flat map. The isochrone reads as a heat wash over the city
      // and accidental two-finger rotation is a common mobile annoyance.
      dragRotate: false,
      pitchWithRotate: false,
      touchPitch: false,
      maxPitch: 0,
      attributionControl: { customAttribution: REPO_ATTR },
    });
    map.touchZoomRotate.disableRotation();
    map.keyboard.disableRotation();
    appliedStyleRef.current = initialStyle;

    // Overlays are rebuilt on every style load — including the first — since
    // a style swap discards all layers.
    const overlays = new MapOverlays(map);
    map.on('style.load', () => overlays.install());
    // The style goes through setStyle (not the constructor) so the zoom-out
    // detail tweak applies to the initial load too.
    map.setStyle(initialStyle.style, {
      transformStyle: (_prev, next) => tuneStyleForZoomOut(next),
    });

    mapRef.current = map;
    overlaysRef.current = overlays;

    return () => {
      map.remove();
      mapRef.current = null;
      overlaysRef.current = null;
    };
  }, []);

  // Swap basemap style when the selection or (for 'default') the system theme changes.
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;

    function applyStyle() {
      const style = resolveMapStyle(stateRef.current.mapStyle);
      if (appliedStyleRef.current === style) return;
      appliedStyleRef.current = style;
      // Full swap, no diff: our overlay layers go down with the old style and
      // come back via the `style.load` handler.
      map!.setStyle(style.style, {
        diff: false,
        transformStyle: (_prev, next) => tuneStyleForZoomOut(next),
      });
    }

    applyStyle();

    if (state.mapStyle === 'default') {
      const mq = window.matchMedia('(prefers-color-scheme: dark)');
      mq.addEventListener('change', applyStyle);
      return () => mq.removeEventListener('change', applyStyle);
    }
  }, [state.mapStyle]);

  // Set up map event handlers
  useEffect(() => {
    const map = mapRef.current;
    const overlays = overlaysRef.current;
    if (!map || !overlays) return;

    function getNodeLatLng(node: number): [number, number] | null {
      const coords = stateRef.current.nodeCoords;
      if (!coords) return null;
      return [coords[node * 2], coords[node * 2 + 1]];
    }

    function clearRouteOverlay() {
      overlays!.clearRoutes();
    }

    function drawRouteSegments(allPaths: HoverPath[]) {
      const lines: RouteFeature[] = [];
      const transfers: PointFeature[] = [];
      const routeColorMap: Record<string, string> = {};
      let colorIdx = 0;
      const seenSegments = new Set<string>();
      const seenTransfers = new Set<string>();
      for (const { segments } of allPaths) {
        for (const seg of segments) {
          // Flat [lat, lon, …]; a segment needs at least two points.
          if (seg.coords.length < 4) continue;
          let color: string;
          let coords = seg.coords;
          if (seg.edgeType === 0) {
            // Normalize walk segment direction so the dedup key collapses
            // walks traversing the same edge in either direction.
            const n = coords.length;
            const [fLat, fLon, lLat, lLon] = [coords[0], coords[1], coords[n - 2], coords[n - 1]];
            if (fLat > lLat || (fLat === lLat && fLon > lLon)) {
              const rev = new Float32Array(n);
              for (let i = 0; i < n; i += 2) {
                rev[i] = coords[n - 2 - i];
                rev[i + 1] = coords[n - 1 - i];
              }
              coords = rev;
            }
            color = '#888';
          } else {
            if (!(seg.routeName in routeColorMap)) {
              // Rust's `TransitRouter::route_color` returns the map-legible hex
              // (already luminance-adjusted via `adjust_color_for_visibility`).
              // Empty string means the route has no GTFS colour — fall back to
              // the palette.
              const s = stateRef.current;
              let routeColor = seg.routeIdx < 0xffffffff ? s.routeColors[seg.routeIdx] || '' : '';
              if (!routeColor) {
                routeColor = ROUTE_COLORS[colorIdx % ROUTE_COLORS.length];
              }
              routeColorMap[seg.routeName] = routeColor;
              colorIdx++;
            }
            color = routeColorMap[seg.routeName];
          }
          const n = coords.length;
          const routeKey = seg.edgeType === 0 ? '' : seg.routeIdx;
          const segKey = `${seg.edgeType}|${routeKey}|${coords[0]},${coords[1]}|${coords[n - 2]},${coords[n - 1]}|${n}`;
          if (!seenSegments.has(segKey)) {
            seenSegments.add(segKey);
            const lngLats: [number, number][] = new Array(n / 2);
            for (let i = 0; i < n; i += 2) lngLats[i / 2] = [coords[i + 1], coords[i]];
            lines.push({
              type: 'Feature',
              properties: { kind: seg.edgeType === 0 ? 'walk' : 'transit', color },
              geometry: { type: 'LineString', coordinates: lngLats },
            });
          }
          // Add a dot at the end of transit segments to mark transfers
          if (seg.edgeType === 1) {
            const s = stateRef.current;
            if (s.nodeCoords && seg.endNodeIdx !== undefined) {
              const tKey = `${seg.routeIdx}|${seg.endNodeIdx}`;
              if (!seenTransfers.has(tKey)) {
                seenTransfers.add(tKey);
                const lat = s.nodeCoords[seg.endNodeIdx * 2];
                const lon = s.nodeCoords[seg.endNodeIdx * 2 + 1];
                transfers.push({
                  type: 'Feature',
                  properties: { color },
                  geometry: { type: 'Point', coordinates: [lon, lat] },
                });
              }
            }
          }
        }
      }
      overlays!.setRoutes(lines, transfers);
    }

    drawRouteLayersRef.current = drawRouteSegments;

    async function showDestination(node: number, pin: boolean) {
      const sAtStart = stateRef.current;
      if (!sAtStart.travelTimes) return;

      // Re-verify state after async work. If the source changed or was
      // cleared, this destination data is stale.
      const s = stateRef.current;
      if (!s.travelTimes || s.sourceNode !== sAtStart.sourceNode) return;

      const hoverData = await buildHoverData(node, s.travelTimes, s.sampleCounts, s.totalSamples);

      // A hover that resolves after the cursor already left the map must not
      // resurrect the cleared hover state. Pins are exempt — they persist.
      if (!pin && !pointerInMap) return;

      // For hovers, skip the imperative route draw if a pin landed during the
      // await. Otherwise the hover's routes would paint over the pin's routes
      // (which were already drawn by the pinned-destination effect). The
      // SET_HOVER_DEST dispatch below still lands harmlessly in hoverDest —
      // rendering uses `pinnedDest ?? hoverDest`, so the pin wins.
      if (pin || stateRef.current.pinnedDest === null) {
        drawRouteSegments(resolveRoutePathsRef.current(hoverData));
      }

      const latLng = getNodeLatLng(node);
      if (!latLng) return;

      if (pin) {
        overlays!.setDest(latLng);
        dispatch({ type: 'PIN_DESTINATION', dest: { node, latLng, hoverData } });
      } else {
        dispatch({ type: 'SET_HOVER_DEST', dest: { node, latLng, hoverData } });
      }
    }

    async function setSource(
      lat: number,
      lng: number,
      opts?: { keepDest?: boolean }
    ): Promise<boolean> {
      const keepDest = opts?.keepDest === true;
      const s = stateRef.current;
      if (s.loadingState !== 'ready') {
        // City data still loading. Queue the click and let App.tsx replay it
        // through this same function once loadingState flips to 'ready'.
        dispatch({ type: 'QUEUE_PENDING_SOURCE', latLng: [lat, lng] });
        return false;
      }
      // Cancel any in-flight query *before* awaiting the worker round-trip
      // for snapToNode — otherwise that message queues behind the running
      // compute and the cancel flag isn't flipped until the compute finishes.
      cancelInflightQuery();
      const node = await snapToNode(lat, lng);
      if (node === null) return false;
      const latLng = getNodeLatLng(node);
      if (!latLng) return false;
      if (sourceMarkerRef.current) {
        sourceMarkerRef.current.setLngLat(toLngLat(latLng));
      } else {
        sourceMarkerRef.current = makeOriginMarker(latLng, 'Origin').addTo(map!);
      }
      // Clear destination unless the caller wants to preserve it (e.g.,
      // setting source via the search bar while a destination is pinned).
      // Route overlay is always cleared — the old routes were drawn against
      // the previous source; App.tsx re-resolves hoverData after the new
      // query completes, which triggers the route-redraw effect.
      if (!keepDest) overlays!.setDest(null);
      clearRouteOverlay();
      dispatch({ type: 'SET_SOURCE', node, latLng, keepDest });
      return true;
    }

    setSourceRef.current = setSource;

    async function setDestination(lat: number, lng: number) {
      const s = stateRef.current;
      if (s.loadingState !== 'ready') {
        // Queue and let App.tsx drain after the first query completes.
        dispatch({ type: 'QUEUE_PENDING_DEST', latLng: [lat, lng] });
        return;
      }
      const node = await snapToNode(lat, lng);
      // Snap failed: clear any existing pin rather than leaving a stale one.
      if (node === null) dispatch({ type: 'UNPIN_DESTINATION' });
      else showDestination(node, true);
    }
    setDestinationRef.current = setDestination;

    // Desktop: double-click sets source
    let lastPinTime = 0;
    // Tracks whether the cursor is currently over the map. Flipped false the
    // moment it leaves (onto a GUI overlay or off-window) so an in-flight
    // hover resolved afterward can bail instead of resurrecting a stale hover.
    let pointerInMap = true;
    function onDblClick(e: MapMouseEvent) {
      // Mobile uses the Origin/Dest toggle in the top bar instead.
      if (isMobileRef.current) return;
      // setSource itself queues if loading isn't ready yet.
      setSource(e.lngLat.lat, e.lngLat.lng);
    }

    // Single click: behavior depends on platform.
    // Desktop: pin/unpin destination. Mobile: routes by interactionMode —
    // 'origin' sets the source, 'dest' pins (or repins) the destination.
    async function onClick(e: MapMouseEvent) {
      const s = stateRef.current;
      const { lat, lng } = e.lngLat;

      if (s.loadingState !== 'ready') {
        // While loading, mobile taps queue an intent. Desktop single-clicks
        // pre-source are a no-op even when ready (only dblclick sets origin),
        // so nothing to queue here.
        if (isMobileRef.current) {
          if (s.interactionMode === 'origin') {
            dispatch({ type: 'QUEUE_PENDING_SOURCE', latLng: [lat, lng] });
          } else {
            dispatch({ type: 'QUEUE_PENDING_DEST', latLng: [lat, lng] });
          }
        }
        return;
      }

      if (isMobileRef.current) {
        if (s.interactionMode === 'origin') {
          setSource(lat, lng);
          return;
        }
        // Dest mode: replace any existing pin with the tapped node. A failed
        // snap clears the pin instead of leaving the previous one behind.
        const node = await snapToNode(lat, lng);
        if (node === null) dispatch({ type: 'UNPIN_DESTINATION' });
        else showDestination(node, true);
        return;
      }

      // Desktop: ignore the second click of a double-click (handled by onDblClick).
      if (e.originalEvent.detail > 1) return;

      if (s.sourceNode === null) return;

      // Desktop: if already pinned, unpin on click (swallow if it's the second
      // click of a double-click so dblclick can set source cleanly).
      if (s.pinnedDest !== null) {
        if (Date.now() - lastPinTime < 300) return;
        dispatch({ type: 'UNPIN_DESTINATION' });
        return;
      }

      const node = await snapToNode(lat, lng);
      if (node !== null) {
        lastPinTime = Date.now();
        showDestination(node, true);
      }
    }

    // Hover: show route (desktop, no pinned dest).
    //
    // Single-flight, latest-wins. Mouse events arrive far faster than the
    // serial router worker can snap a point and build hover data, and the
    // hover-data build (Pareto paths + display strings) is the expensive
    // part. Awaiting a snap per event flooded the worker with requests for
    // nodes the cursor had already left, and every one of them then drew
    // routes, re-rendered the panel and repainted the map. Now a move only
    // records the newest pointer position; one job at a time snaps it and,
    // if it landed on a new node, builds and draws that node. Positions that
    // arrive while a job runs collapse into a single follow-up job, so the
    // worker never holds more than one hover request.
    let hoverTarget: { lat: number; lng: number } | null = null;
    let hoverJobRunning = false;
    /** Last pointer position in container pixels, to re-hover after a pan. */
    let lastPointerPoint: { x: number; y: number } | null = null;

    function onMouseMove(e: MapMouseEvent) {
      pointerInMap = true;
      lastPointerPoint = { x: e.point.x, y: e.point.y };
      const s = stateRef.current;
      if (!s.travelTimes || s.pinnedDest !== null) return;
      // MapLibre fires mousemove throughout a drag-pan. The user is panning,
      // not pointing, and a hover-data stall mid-pan is what reads as lag —
      // so wait for the camera to settle (see onMoveEnd).
      if (map!.isMoving()) return;
      hoverTarget = { lat: e.lngLat.lat, lng: e.lngLat.lng };
      void pumpHover();
    }

    async function pumpHover() {
      if (hoverJobRunning) return;
      hoverJobRunning = true;
      try {
        while (hoverTarget) {
          const { lat, lng } = hoverTarget;
          hoverTarget = null;
          await hoverAt(lat, lng);
        }
      } finally {
        hoverJobRunning = false;
      }
    }

    async function hoverAt(lat: number, lng: number) {
      const node = await snapToNode(lat, lng);
      // The cursor may have left the map during the snap round-trip — bail so
      // we don't re-show a hover that onMouseOut has already cleared.
      if (!pointerInMap) return;
      if (node === null) {
        // Cursor is over a spot with no graph node nearby — drop the hover so
        // a stale destination doesn't linger while pointing at empty space.
        if (lastHoveredNodeRef.current !== null) {
          lastHoveredNodeRef.current = null;
          clearRouteOverlay();
          dispatch({ type: 'CLEAR_HOVER' });
        }
        return;
      }
      if (node === lastHoveredNodeRef.current) return;
      lastHoveredNodeRef.current = node;
      // Awaited on purpose: the hover-data build must finish before the next
      // pointer position is processed, or requests pile up in the worker.
      await showDestination(node, false);
    }

    function onMouseOut(e: MouseEvent) {
      pointerInMap = false;
      hoverTarget = null;
      // Moving onto our own HoverInfo panel is not "leaving onto another GUI
      // element" — it's the panel showing this very hover. Clearing there
      // causes a flicker loop: snapping a destination grows the panel under
      // the cursor, which fires this mouseleave, which would clear the dest,
      // which shrinks the panel back off the cursor… forever. So keep the
      // hover (and its node ref) when the pointer lands on the panel.
      const related = e.relatedTarget;
      if (related instanceof Element && related.closest('#hover-info')) return;
      lastHoveredNodeRef.current = null;
      if (stateRef.current.pinnedDest === null) {
        clearRouteOverlay();
        dispatch({ type: 'CLEAR_HOVER' });
      }
    }

    // Frame renderer registered with the animation store. The rAF playback
    // loop (and seek/step) call this directly with each departure-time frame,
    // bypassing React entirely. The layer copies the frame and asks the map
    // for a repaint; pan/zoom needs no re-render since the layer draws inside
    // the map's own frame loop.
    function renderFrame(frame: Uint16Array) {
      const s = stateRef.current;
      if (!s.nodeCoords) return;
      overlays!.iso.setNodes(s.nodeCoords);
      overlays!.iso.setFrame(frame);
    }
    animationStore.setFrameRenderer(renderFrame);

    function onMoveEnd() {
      // Camera settled: hover whatever is now under the (stationary) pointer.
      const s = stateRef.current;
      if (pointerInMap && lastPointerPoint && s.travelTimes && s.pinnedDest === null) {
        const ll = map!.unproject([lastPointerPoint.x, lastPointerPoint.y]);
        hoverTarget = { lat: ll.lat, lng: ll.lng };
        void pumpHover();
      }
      if (s.sourceNode === null) return;
      const c = map!.getCenter();
      const current = getHashParams();
      setHashParams({
        ...current,
        zoom: mapToSlippyZoom(map!.getZoom()),
        center: [c.lat, c.lng],
      });
    }

    map.on('dblclick', onDblClick);
    map.on('click', onClick);
    map.on('mousemove', onMouseMove);
    map.getContainer().addEventListener('mouseleave', onMouseOut);
    map.on('moveend', onMoveEnd);

    return () => {
      map.off('dblclick', onDblClick);
      map.off('click', onClick);
      map.off('mousemove', onMouseMove);
      map.getContainer().removeEventListener('mouseleave', onMouseOut);
      map.off('moveend', onMoveEnd);
      animationStore.setFrameRenderer(null);
    };
  }, [dispatch]);

  // Reposition map on city change. Runs as soon as the city is known so the
  // base map is centered + bbox-framed during the data download/init, not
  // after — users can pan/zoom while the .bin loads.
  useEffect(() => {
    const map = mapRef.current;
    const overlays = overlaysRef.current;
    const city = state.currentCity;
    if (!map || !overlays || !city) return;

    const hashParams = getHashParams();
    if (hashParams.center && hashParams.zoom !== undefined) {
      map.jumpTo({ center: toLngLat(hashParams.center), zoom: slippyToMapZoom(hashParams.zoom) });
    } else {
      map.jumpTo({ center: toLngLat(city.center), zoom: city.zoom });
    }

    overlays.setBbox(city.bbox);

    // Clean up old overlays
    if (sourceMarkerRef.current) {
      sourceMarkerRef.current.remove();
      sourceMarkerRef.current = null;
    }
    overlays.setDest(null);
    overlays.clearRoutes();
    overlays.iso.clear();
    // Only refit on city change. Refitting on every loadingState transition
    // would yank the map back from any panning the user did during load.
  }, [state.currentCity]);

  // Feed the isochrone layer. In the average view it draws the query result;
  // in playback ('frame' mode) the animation store pushes frames through
  // `renderFrame` instead and this effect only keeps the budget current.
  // Entering 'frame' mode needs no action here: the store's enter() pushes the
  // first frame itself.
  const animMode = useAnimMode();
  const animDep = useAnimRenderedDeparture();
  useEffect(() => {
    const iso = overlaysRef.current?.iso;
    if (!iso) return;
    iso.setMaxTime(state.maxTimeMin * 60);
    if (!state.travelTimes || !state.nodeCoords) {
      iso.clear();
      return;
    }
    iso.setNodes(state.nodeCoords);
    if (animMode === 'average') {
      iso.setAverage(state.travelTimes, state.sampleCounts, state.totalSamples);
    }
  }, [
    state.travelTimes,
    state.sampleCounts,
    state.totalSamples,
    state.nodeCoords,
    state.maxTimeMin,
    animMode,
  ]);

  // Keep the route-path resolver current. In average mode the map shows the
  // whole Pareto fan; in playback it shows only the path optimal for the
  // rendered departure — the same path the HoverInfo panel describes.
  resolveRoutePathsRef.current = (hd: HoverData): HoverPath[] => {
    if (animMode === 'frame') {
      const p = deriveDisplayPath(
        hd,
        animDep,
        state.windowStart,
        state.windowEnd,
        state.maxTimeMin * 60
      );
      return p && p.segments.length > 0 ? [p] : [];
    }
    return hd.allPaths.filter((p) => p.segments.length > 0);
  };

  // Destination pin follows pinnedDest: shown when set (including URL
  // restore), hidden with its routes when unpinned.
  useEffect(() => {
    const overlays = overlaysRef.current;
    if (!overlays) return;
    if (state.pinnedDest === null) {
      overlays.setDest(null);
      overlays.clearRoutes();
      return;
    }
    overlays.setDest(state.pinnedDest.latLng);
  }, [state.pinnedDest]);

  // Redraw the pinned destination's routes whenever its hover data changes or
  // the animation playhead moves. In playback this swaps the drawn route to
  // the one optimal for the new departure time; in the average view it redraws
  // the full Pareto fan. Hover routes are handled imperatively in
  // `showDestination`, not here.
  useEffect(() => {
    const pinnedHoverData = state.pinnedDest?.hoverData;
    if (!drawRouteLayersRef.current || !pinnedHoverData) return;
    drawRouteLayersRef.current(resolveRoutePathsRef.current(pinnedHoverData));
  }, [state.pinnedDest, animMode, animDep]);

  // Draw source marker when sourceNode is set externally (URL restore)
  useEffect(() => {
    const { sourceNode, sourceLatLng } = state;
    if (sourceNode === null || !sourceLatLng || !mapRef.current) return;
    if (sourceMarkerRef.current) return;
    sourceMarkerRef.current = makeOriginMarker(sourceLatLng, 'Origin').addTo(mapRef.current);
  }, [state.sourceNode, state.sourceLatLng]);

  // Provisional source marker. Visible only while a click is queued during
  // load; removed as soon as a real source marker replaces it (or pending
  // clears on city change / load error / consumption).
  useEffect(() => {
    if (!mapRef.current) return;
    const pending = state.pendingSource;
    if (!pending || state.sourceNode !== null) {
      if (provisionalSourceRef.current) {
        provisionalSourceRef.current.remove();
        provisionalSourceRef.current = null;
      }
      return;
    }
    if (provisionalSourceRef.current) {
      provisionalSourceRef.current.setLngLat(toLngLat(pending.latLng));
    } else {
      provisionalSourceRef.current = makeOriginMarker(
        pending.latLng,
        'Origin (queued)',
        0.45
      ).addTo(mapRef.current);
    }
  }, [state.pendingSource, state.sourceNode]);

  return <div id="map" ref={mapContainerRef} />;
});

export default MapView;
