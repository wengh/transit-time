import React, { useState } from 'react';
import { useAppState } from '../state/AppContext';
import PathSegmentList from './PathSegmentList';
import { TripChart, deriveDisplayPath, deriveTitleText } from './HoverInfo';

// Bottom info strip + expandable drawer. Only renders when a destination is
// pinned (mobile has no hover, so `state.hoverData` is effectively the pin).
// Collapsed = 56px summary line; expanded = ~68vh with route segment list and
// the sawtooth chart in 5:2 aspect ratio.
export default function MobileBottomSheet(): React.ReactNode {
  const { state, dispatch } = useAppState();
  const [expanded, setExpanded] = useState(false);

  if (state.loadingState !== 'ready' || !state.hoverData || state.pinnedNode === null) {
    return null;
  }

  const displayPath = deriveDisplayPath(state.hoverData, state.selectedSampleIdx);
  const titleText = deriveTitleText(state.hoverData, state.selectedSampleIdx, displayPath);

  function toggle() {
    setExpanded((v) => !v);
  }

  function unpin(e: React.MouseEvent) {
    e.stopPropagation();
    dispatch({ type: 'UNPIN_DESTINATION' });
    setExpanded(false);
  }

  return (
    <div
      className={`fixed left-0 right-0 bottom-0 z-[1100]
        bg-[rgba(18,18,20,0.97)] backdrop-blur-md
        border-t border-zinc-800 text-zinc-100
        rounded-t-xl shadow-[0_-4px_16px_rgba(0,0,0,0.5)]
        transition-[max-height] duration-200 ease-out
        flex flex-col overflow-hidden
        pb-[max(env(safe-area-inset-bottom),0.5rem)]`}
      style={{ maxHeight: expanded ? '68vh' : '56px' }}
    >
      <button
        onClick={toggle}
        aria-label={expanded ? 'Collapse details' : 'Expand details'}
        className="flex flex-col items-stretch text-left px-3 pt-1.5 pb-1
          flex-shrink-0 select-none"
      >
        <div className="self-center w-9 h-1 rounded-full bg-zinc-600 mb-1.5" />
        <div className="flex items-center gap-2">
          <div className="text-[13px] flex-1 min-w-0 truncate">{titleText}</div>
          <span
            role="button"
            tabIndex={0}
            onClick={unpin}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') unpin(e as any);
            }}
            className="text-[12px] text-zinc-400 px-2 py-0.5 rounded
              hover:bg-zinc-800 active:bg-zinc-700 cursor-pointer"
          >
            Clear
          </span>
        </div>
      </button>

      {expanded && (
        <div className="overflow-y-auto px-3 pb-3 text-[12px]">
          {displayPath && displayPath.segments.length > 0 && <PathSegmentList path={displayPath} />}
          <div className="mt-2">
            <TripChart aspectRatio="5/2" />
          </div>
        </div>
      )}
    </div>
  );
}
