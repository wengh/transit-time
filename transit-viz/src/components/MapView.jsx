import { useEffect, useRef } from 'react';
import L from 'leaflet';
import { useAppState } from '../state/AppContext.jsx';
import { initWebGL, renderIsochrone } from '../utils/webgl.js';
import { getHoverData } from '../utils/router.js';
import { ROUTE_COLORS } from '../utils/colors.js';

const isTouchDevice = typeof window !== 'undefined' && ('ontouchstart' in window || navigator.maxTouchPoints > 0);

export default function MapView() {
  const { state, dispatch } = useAppState();
  const mapRef = useRef(null);
  const mapContainerRef = useRef(null);
  const glStateRef = useRef(null);
  const isoOverlayRef = useRef(null);
  const sourceMarkerRef = useRef(null);
  const destMarkerRef = useRef(null);
  const routePolylinesRef = useRef([]);
  const lastHoveredNodeRef = useRef(null);
  const longPressTimerRef = useRef(null);
  const longPressStartRef = useRef(null);

  // Keep current state in refs for event handlers
  const stateRef = useRef(state);
  stateRef.current = state;

  // Initialize map
  useEffect(() => {
    if (mapRef.current) return;
    const map = L.map('map', { doubleClickZoom: isTouchDevice }).setView([40, -90], 4);
    L.tileLayer('https://{s}.basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}{r}.png', {
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>',
      maxZoom: 20,
      subdomains: 'abcd',
      crossOrigin: true,
    }).addTo(map);
    mapRef.current = map;

    return () => { map.remove(); mapRef.current = null; };
  }, []);

  // Set up map event handlers
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;

    function snapToNode(lat, lon) {
      const router = stateRef.current.router;
      if (!router) return null;
      return router.snap_to_node(lat, lon);
    }

    function getNodeLatLng(node) {
      const coords = stateRef.current.nodeCoords;
      if (!coords) return null;
      return [coords[node * 2], coords[node * 2 + 1]];
    }

    function clearRouteOverlay() {
      routePolylinesRef.current.forEach(p => p.remove());
      routePolylinesRef.current = [];
    }

    function drawRouteSegments(allPaths) {
      clearRouteOverlay();
      const routeColorMap = {};
      let colorIdx = 0;
      for (const { segments } of allPaths) {
        for (const seg of segments) {
          if (seg.coords.length < 2) continue;
          let color, dashArray, weight;
          if (seg.edgeType === 0) {
            color = '#888'; dashArray = '6, 8'; weight = 3;
          } else {
            if (!(seg.routeName in routeColorMap)) {
              routeColorMap[seg.routeName] = ROUTE_COLORS[colorIdx % ROUTE_COLORS.length];
              colorIdx++;
            }
            color = routeColorMap[seg.routeName]; dashArray = null; weight = 4;
          }
          const line = L.polyline(seg.coords, { color, weight, opacity: 1, dashArray, interactive: false }).addTo(map);
          routePolylinesRef.current.push(line);
        }
      }
    }

    function showDestination(node, pin) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || !s.ssspList) return;
      const tt = s.travelTimes[node];
      if (isNaN(tt) || tt < 0) {
        clearRouteOverlay();
        if (destMarkerRef.current) { destMarkerRef.current.remove(); destMarkerRef.current = null; }
        dispatch({ type: pin ? 'UNPIN_DESTINATION' : 'CLEAR_HOVER' });
        return;
      }

      const allPaths = getHoverData(s.router, s.ssspList, node);
      const travelTimes = allPaths
        .map(p => p.totalTime)
        .filter(t => t !== null && isFinite(t))
        .sort((a, b) => a - b);

      drawRouteSegments(allPaths.filter(p => p.segments.length > 0));

      const latLng = getNodeLatLng(node);
      if (pin) {
        if (destMarkerRef.current) {
          destMarkerRef.current.setLatLng(latLng);
        } else {
          destMarkerRef.current = L.circleMarker(latLng, {
            radius: 6, color: '#fff', fillColor: '#4a90d9', fillOpacity: 1, weight: 2,
          }).addTo(map);
        }
        dispatch({ type: 'PIN_DESTINATION', node, latLng, hoverData: { allPaths, travelTimes } });
      } else {
        dispatch({ type: 'SET_HOVER_DATA', hoverData: { allPaths, travelTimes } });
      }
    }

    function setSource(lat, lng) {
      const s = stateRef.current;
      if (!s.router) return;
      const node = snapToNode(lat, lng);
      const latLng = getNodeLatLng(node);
      if (sourceMarkerRef.current) {
        sourceMarkerRef.current.setLatLng(latLng);
      } else {
        sourceMarkerRef.current = L.marker(latLng, { title: 'Origin' }).addTo(map);
      }
      // Clear destination
      if (destMarkerRef.current) { destMarkerRef.current.remove(); destMarkerRef.current = null; }
      clearRouteOverlay();
      dispatch({ type: 'SET_SOURCE', node, latLng });
    }

    // Desktop: double-click sets source
    let clickTimer = null;
    function onDblClick(e) {
      if (!stateRef.current.router) return;
      // Prevent on mobile (handled by long press)
      if (isTouchDevice) return;
      // Cancel pending single-click
      if (clickTimer) { clearTimeout(clickTimer); clickTimer = null; }
      setSource(e.latlng.lat, e.latlng.lng);
    }

    // Single click: pin/unpin destination (delayed on desktop to distinguish from dblclick)
    function onClick(e) {
      const s = stateRef.current;
      if (!s.router || s.sourceNode === null) return;

      // Ignore if this was part of a long press
      if (longPressStartRef.current) {
        const elapsed = Date.now() - longPressStartRef.current;
        longPressStartRef.current = null;
        if (elapsed > 400) return;
      }

      function doClick() {
        const s = stateRef.current;
        if (s.pinnedNode !== null) {
          if (destMarkerRef.current) { destMarkerRef.current.remove(); destMarkerRef.current = null; }
          clearRouteOverlay();
          dispatch({ type: 'UNPIN_DESTINATION' });
        } else {
          const node = snapToNode(e.latlng.lat, e.latlng.lng);
          showDestination(node, true);
        }
      }

      if (isTouchDevice) {
        doClick();
      } else {
        // Delay to allow dblclick to cancel
        clickTimer = setTimeout(() => { clickTimer = null; doClick(); }, 250);
      }
    }

    // Hover: show route (desktop, no pinned dest)
    function onMouseMove(e) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || !s.ssspList || s.pinnedNode !== null) return;

      const node = snapToNode(e.latlng.lat, e.latlng.lng);
      if (node === lastHoveredNodeRef.current) return;
      lastHoveredNodeRef.current = node;
      showDestination(node, false);
    }

    function onMouseOut() {
      lastHoveredNodeRef.current = null;
      if (stateRef.current.pinnedNode === null) {
        clearRouteOverlay();
        dispatch({ type: 'CLEAR_HOVER' });
      }
    }

    // Mobile: long press to set source
    function onTouchStart(e) {
      if (e.originalEvent.touches.length !== 1) return;
      const touch = e.originalEvent.touches[0];
      longPressStartRef.current = Date.now();
      longPressTimerRef.current = setTimeout(() => {
        if (!stateRef.current.router) return;
        const latLng = map.containerPointToLatLng([touch.clientX, touch.clientY]);
        setSource(latLng.lat, latLng.lng);
        longPressStartRef.current = null;
      }, 600);
    }

    function onTouchEnd() {
      if (longPressTimerRef.current) {
        clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = null;
      }
    }

    function onTouchMove() {
      if (longPressTimerRef.current) {
        clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = null;
      }
      longPressStartRef.current = null;
    }

    // Re-render isochrone on map move/zoom
    function onMoveEnd() {
      renderIso();
    }

    function renderIso() {
      const s = stateRef.current;
      if (!s.travelTimes || !s.nodeCoords) return;
      if (!glStateRef.current) {
        glStateRef.current = initWebGL();
      }
      if (!glStateRef.current) return;
      const result = renderIsochrone(glStateRef.current, map, s.travelTimes, s.nodeCoords, s.maxTimeMin * 60, L);
      if (result) {
        const oldOverlay = isoOverlayRef.current;
        isoOverlayRef.current = L.imageOverlay(result.dataUrl, result.renderBounds, {
          opacity: 1, interactive: false, zIndex: 500,
        }).addTo(map);
        if (oldOverlay) map.removeLayer(oldOverlay);
      }
    }

    // Store renderIso on the map element for external trigger
    map._renderIso = renderIso;

    map.on('dblclick', onDblClick);
    map.on('click', onClick);
    map.on('mousemove', onMouseMove);
    map.on('mouseout', onMouseOut);
    map.on('moveend', onMoveEnd);
    map.on('zoomend', onMoveEnd);

    // Touch events on the map container
    const container = map.getContainer();
    const touchStartHandler = (e) => onTouchStart({ originalEvent: e });
    container.addEventListener('touchstart', touchStartHandler, { passive: true });
    container.addEventListener('touchend', onTouchEnd, { passive: true });
    container.addEventListener('touchmove', onTouchMove, { passive: true });

    return () => {
      if (clickTimer) clearTimeout(clickTimer);
      map.off('dblclick', onDblClick);
      map.off('click', onClick);
      map.off('mousemove', onMouseMove);
      map.off('mouseout', onMouseOut);
      map.off('moveend', onMoveEnd);
      map.off('zoomend', onMoveEnd);
      container.removeEventListener('touchstart', touchStartHandler);
      container.removeEventListener('touchend', onTouchEnd);
      container.removeEventListener('touchmove', onTouchMove);
    };
  }, [dispatch]);

  // Reposition map on city change
  useEffect(() => {
    const map = mapRef.current;
    const city = state.currentCity;
    if (!map || !city || state.loadingState !== 'ready') return;

    map.setView(city.center, city.zoom);
    // Clean up old overlays
    if (sourceMarkerRef.current) { sourceMarkerRef.current.remove(); sourceMarkerRef.current = null; }
    if (destMarkerRef.current) { destMarkerRef.current.remove(); destMarkerRef.current = null; }
    if (isoOverlayRef.current) { map.removeLayer(isoOverlayRef.current); isoOverlayRef.current = null; }
    routePolylinesRef.current.forEach(p => p.remove());
    routePolylinesRef.current = [];
  }, [state.currentCity, state.loadingState]);

  // Re-render isochrone when travel times or max time changes
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !map._renderIso) return;
    map._renderIso();
  }, [state.travelTimes, state.maxTimeMin]);

  return <div id="map" ref={mapContainerRef} />;
}
