import React, { useRef } from 'react';
import { useAppState } from '../state/AppContext';
import ControlsBody from './ControlsBody';

interface ControlsProps {
  onRunQuery: (overrides?: Record<string, any>) => void;
  onCopy: () => void;
  isFront: boolean;
  onActivate: () => void;
}

// Desktop-only positioned panel. Mobile UI uses MobileSettingsSheet instead,
// rendered conditionally from App.tsx.
export default function Controls({
  onRunQuery,
  onCopy,
  isFront,
  onActivate,
}: ControlsProps): React.ReactNode {
  const { state } = useAppState();
  const justActivatedRef = useRef(false);

  if (state.loadingState !== 'ready') return null;

  return (
    <div
      id="controls"
      onPointerDownCapture={(e) => {
        if (!isFront) {
          justActivatedRef.current = true;
          onActivate();
          e.stopPropagation();
          e.preventDefault();
        }
      }}
      onClickCapture={(e) => {
        if (justActivatedRef.current) {
          justActivatedRef.current = false;
          e.stopPropagation();
          e.preventDefault();
        }
      }}
      className={[
        `absolute ${isFront ? 'z-[1001]' : 'z-[1000]'}`,
        'top-2.5 right-2.5',
        'min-w-[280px] max-h-[calc(100vh-20px)] overflow-y-auto',
        'rounded-lg p-4',
        'bg-white/95 dark:bg-zinc-900/95',
        'shadow-[0_2px_12px_rgba(0,0,0,0.5)]',
      ].join(' ')}
    >
      <ControlsBody onRunQuery={onRunQuery} onCopy={onCopy} />
    </div>
  );
}
