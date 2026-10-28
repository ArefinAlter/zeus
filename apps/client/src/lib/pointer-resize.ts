/** A pointer drag ends on release, cancellation, capture loss, blur, or caller cleanup. */
export function pointerResize(
  event: Pick<PointerEvent, 'button' | 'isPrimary' | 'pointerId' | 'clientX' | 'clientY' | 'preventDefault' | 'stopPropagation'>,
  grip: HTMLElement,
  options: { axis: 'x' | 'y'; direction?: 1 | -1; initial: number; bounds: () => { min: number; max: number }; change: (value: number) => void; finish?: (value: number, cancelled: boolean) => void },
): () => void {
  if (event.button !== 0 || event.isPrimary === false) return () => {};
  event.preventDefault();
  event.stopPropagation();
  const id = event.pointerId;
  const start = options.axis === 'x' ? event.clientX : event.clientY;
  const listeners = new AbortController();
  const root = document.documentElement;
  const oldCursor = root.style.cursor;
  let value = options.initial;
  let ended = false;
  root.classList.add('panel-resizing');
  root.style.cursor = options.axis === 'x' ? 'col-resize' : 'row-resize';
  try { grip.setPointerCapture(id); } catch { /* Global listeners still cover a detached test surface. */ }
  const stop = (cancelled: boolean) => {
    if (ended) return;
    ended = true;
    listeners.abort();
    try { if (grip.hasPointerCapture(id)) grip.releasePointerCapture(id); } catch { /* It may have unmounted. */ }
    root.classList.remove('panel-resizing');
    root.style.cursor = oldCursor;
    options.finish?.(value, cancelled);
  };
  const update = (e: PointerEvent) => {
    if (e.pointerId !== id || !Number.isFinite(e.clientX) || !Number.isFinite(e.clientY)) return;
    const { min, max } = options.bounds();
    const point = options.axis === 'x' ? e.clientX : e.clientY;
    value = Math.round(Math.max(min, Math.min(Math.max(min, max), options.initial + (point - start) * (options.direction ?? 1))));
    options.change(value);
  };
  const signal = listeners.signal;
  window.addEventListener('pointermove', update, { signal });
  window.addEventListener('pointerup', (e) => { if (e.pointerId === id) { update(e); stop(false); } }, { signal });
  window.addEventListener('pointercancel', (e) => { if (e.pointerId === id) stop(true); }, { signal });
  grip.addEventListener('lostpointercapture', (e) => { if (e.pointerId === id) stop(true); }, { signal });
  window.addEventListener('blur', () => stop(true), { signal });
  window.addEventListener('keydown', (e) => { if (e.key === 'Escape') { e.preventDefault(); stop(true); } }, { signal });
  window.addEventListener('resize', () => stop(true), { signal });
  return () => stop(true);
}
