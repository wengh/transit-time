import { useEffect, useState } from 'react';

// Tailwind's `sm:` breakpoint is 640px, so anything strictly below that is the
// mobile UI surface. Subscribe to the media query so rotating / resizing the
// viewport flips the React tree between mobile and desktop layouts.
const QUERY = '(max-width: 639px)';

export function useIsMobile(): boolean {
  const [isMobile, setIsMobile] = useState<boolean>(() => {
    if (typeof window === 'undefined') return false;
    return window.matchMedia(QUERY).matches;
  });

  useEffect(() => {
    const mq = window.matchMedia(QUERY);
    const onChange = () => setIsMobile(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);

  return isMobile;
}
