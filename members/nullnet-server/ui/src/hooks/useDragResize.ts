import { useCallback, useRef, useState } from 'react';

export function useDragResize(initialWidth: number, min = 180, max = 600) {
  const [width, setWidth] = useState(initialWidth);
  const widthRef = useRef(width);
  widthRef.current = width;

  const onResizeStart = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startX = e.clientX;
      const startWidth = widthRef.current;

      const onMove = (ev: MouseEvent) => {
        setWidth(Math.min(max, Math.max(min, startWidth + (startX - ev.clientX))));
      };

      const onUp = () => {
        document.removeEventListener('mousemove', onMove);
        document.removeEventListener('mouseup', onUp);
        document.body.style.cursor = '';
        document.body.style.userSelect = '';
      };

      document.body.style.cursor = 'col-resize';
      document.body.style.userSelect = 'none';
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
    },
    [min, max],
  );

  return { width, onResizeStart };
}
