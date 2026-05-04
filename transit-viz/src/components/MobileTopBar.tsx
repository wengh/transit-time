import React from 'react';
import type { RefObject } from 'react';
import { useAppState } from '../state/AppContext';
import { deriveStatusText } from '../utils/statusText';
import LocationSearch from './LocationSearch';
import type { MapViewHandle } from './MapView';

interface MobileTopBarProps {
  onOpenSettings: () => void;
  mapViewRef: RefObject<MapViewHandle | null>;
}

export default function MobileTopBar({
  onOpenSettings,
  mapViewRef,
}: MobileTopBarProps): React.ReactNode {
  const { state, dispatch } = useAppState();
  if (state.loadingState !== 'ready') return null;

  const { interactionMode, currentCity } = state;
  const hint = deriveStatusText(state, { mobile: true });

  function setMode(mode: 'origin' | 'dest') {
    dispatch({ type: 'SET_INTERACTION_MODE', mode });
  }

  // Visual styling: pill segmented control. Active segment uses zinc-100 on a
  // dark backdrop; inactive is muted text on transparent. Reads well over both
  // light and dark map tiles thanks to the blurred bar background.
  const segBase = 'flex-1 px-3 py-1.5 text-[13px] font-medium transition-colors select-none';
  const segActive = 'bg-zinc-100 text-zinc-900';
  const segInactive = 'text-zinc-300';

  return (
    <div
      className="fixed top-0 left-0 right-0 z-[1100]
        bg-[rgba(18,18,20,0.95)] backdrop-blur-md
        border-b border-zinc-800
        px-3 pt-[max(env(safe-area-inset-top),0.5rem)] pb-2
        flex flex-col gap-1.5"
    >
      <div className="flex items-center gap-3">
        <div className="text-zinc-100 text-[14px] font-semibold truncate flex-1 min-w-0">
          {currentCity?.name ?? ''}
        </div>
        <div
          role="tablist"
          className="flex rounded-full overflow-hidden bg-zinc-800 border border-zinc-700"
        >
          <button
            role="tab"
            aria-selected={interactionMode === 'origin'}
            onClick={() => setMode('origin')}
            className={`${segBase} rounded-l-full ${
              interactionMode === 'origin' ? segActive : segInactive
            }`}
          >
            Origin
          </button>
          <button
            role="tab"
            aria-selected={interactionMode === 'dest'}
            onClick={() => setMode('dest')}
            className={`${segBase} rounded-r-full ${
              interactionMode === 'dest' ? segActive : segInactive
            }`}
          >
            Dest
          </button>
        </div>
        <button
          aria-label="Settings"
          onClick={onOpenSettings}
          className="w-9 h-9 flex items-center justify-center rounded-full
            text-zinc-200 hover:bg-zinc-800 active:bg-zinc-700"
        >
          <svg
            viewBox="0 0 24 24"
            width="20"
            height="20"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h0a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51h0a1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v0a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
      <LocationSearch mapViewRef={mapViewRef} variant="mobile" />
      <div className="text-[11px] text-zinc-500 truncate">{hint}</div>
    </div>
  );
}
