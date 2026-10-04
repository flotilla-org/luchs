import Foundation

// The document scroller is the only scroll target in v1. Pointer tracking lives
// in the page so CSS inheritance, editable content and text hit testing agree
// with the content WebKit draws.
let pageAffordancesScript = """
(() => {
  let pointer = null, cursor = null, scroll = null, queued = false;
  const post = (domain, body) => window.webkit.messageHandlers.luchsState.postMessage({domain, body});
  const cursors = new Set(['default','none','context-menu','help','pointer','progress','wait','cell','crosshair',
    'text','vertical-text','alias','copy','move','no-drop','not-allowed','grab','grabbing','all-scroll',
    'col-resize','row-resize','n-resize','e-resize','s-resize','w-resize','ne-resize','nw-resize',
    'se-resize','sw-resize','ew-resize','ns-resize','nesw-resize','nwse-resize','zoom-in','zoom-out']);
  const shape = () => {
    if (!pointer) return 'default';
    const element = document.elementFromPoint(pointer.x, pointer.y);
    if (!element) return 'default';
    let value = getComputedStyle(element).cursor;
    if (value === 'auto') {
      if (element.closest('a[href],area[href]')) return 'pointer';
      if (element.isContentEditable || element.closest('textarea,input:not([type]),input[type=text],input[type=search],input[type=url],input[type=tel],input[type=email],input[type=password]')) return 'text';
      const range = document.caretRangeFromPoint(pointer.x, pointer.y);
      if (range && range.startContainer.nodeType === Node.TEXT_NODE) {
        const text = range.startContainer;
        if (text.textContent.trim() && text.length) {
          const offset = Math.min(range.startOffset, text.length - 1);
          range.setStart(text, offset); range.setEnd(text, offset + 1);
          const r = range.getBoundingClientRect();
          if (pointer.x >= r.left && pointer.x <= r.right && pointer.y >= r.top && pointer.y <= r.bottom) return 'text';
        }
      }
      return 'default';
    }
    return cursors.has(value) ? value : 'default';
  };
  const axis = (element, horizontal) => {
    const content = horizontal ? element.scrollWidth : element.scrollHeight;
    const viewport = horizontal ? element.clientWidth : element.clientHeight;
    const position = horizontal ? element.scrollLeft : element.scrollTop;
    const style = getComputedStyle(element);
    let overflow = horizontal ? style.overflowX : style.overflowY;
    // Body overflow propagates to the viewport when the root is visible.
    if (element === document.documentElement && overflow === 'visible' && document.body) {
      const bodyStyle = getComputedStyle(document.body);
      overflow = horizontal ? bodyStyle.overflowX : bodyStyle.overflowY;
    }
    const scrollable = content > viewport && !['hidden','clip'].includes(overflow);
    return {scrollable, content_length:content, viewport_length:viewport,
      position:scrollable ? Math.min(Math.max(0, position), Math.max(0, content - viewport)) : 0};
  };
  const update = () => {
    queued = false;
    const nextCursor = shape();
    if (nextCursor !== cursor) { cursor = nextCursor; post('cursor', {shape:cursor}); }
    const element = document.scrollingElement;
    if (element) {
      const body = {x:axis(element, true), y:axis(element, false), capabilities:{}};
      const nextScroll = JSON.stringify(body);
      if (nextScroll !== scroll) { scroll = nextScroll; post('scroll', body); }
    }
  };
  const schedule = () => {
    if (!queued) { queued = true; requestAnimationFrame(update); }
  };
  for (const event of ['mousemove','mousedown']) window.addEventListener(event, e => { pointer = {x:e.clientX, y:e.clientY}; schedule(); }, true);
  document.documentElement.addEventListener('mouseleave', () => { pointer = null; schedule(); });
  for (const event of ['scroll','resize','load','transitionend','animationend']) window.addEventListener(event, schedule, true);
  const observer = new ResizeObserver(schedule);
  observer.observe(document.documentElement);
  if (document.body) observer.observe(document.body);
  new MutationObserver(schedule).observe(document, {subtree:true, childList:true, attributes:true, characterData:true});
  window.__luchsPublishPageState = () => { cursor = null; scroll = null; schedule(); };
  window.addEventListener('pageshow', window.__luchsPublishPageState);
  window.__luchsScrollCommand = command => {
    const element = document.scrollingElement;
    if (!element) return;
    const horizontal = command.axis === 'x';
    const current = axis(element, horizontal);
    const delta = (command.step === 'large' ? current.viewport_length * 0.9 : 40) * (command.direction === 'increment' ? 1 : -1);
    const desired = command.type === 'scroll.set_position' ? command.position : current.position + delta;
    const position = current.scrollable ? Math.min(Math.max(0, desired), Math.max(0, current.content_length - current.viewport_length)) : 0;
    element.scrollTo({left:horizontal ? position : element.scrollLeft,
      top:horizontal ? element.scrollTop : position, behavior:'instant'});
    schedule();
  };
  update();
})();
"""

/// Defense at the engine boundary, independent of the Rust host-verb filter.
func allowedNavigationURL(_ value: String, startupPage: URL) -> URL? {
    guard let url = URL(string: value), let scheme = url.scheme?.lowercased() else { return nil }
    if scheme == "http" || scheme == "https" {
        return url.host?.isEmpty == false ? url : nil
    }
    guard scheme == "file", startupPage.isFileURL,
          url.host == nil || url.host == "" || url.host == "localhost",
          FileManager.default.fileExists(atPath: url.path) else { return nil }
    let directory = startupPage.resolvingSymlinksInPath().deletingLastPathComponent().standardizedFileURL.pathComponents
    let destination = url.resolvingSymlinksInPath().standardizedFileURL.pathComponents
    guard destination.starts(with: directory) else { return nil }
    return url
}
