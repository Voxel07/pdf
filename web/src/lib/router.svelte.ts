// Minimal history-API router.
export const route = $state({ path: location.pathname, query: new URLSearchParams(location.search) });

export function navigate(to: string, replace = false) {
  if (replace) history.replaceState(null, '', to);
  else history.pushState(null, '', to);
  const u = new URL(to, location.origin);
  route.path = u.pathname;
  route.query = u.searchParams;
  window.scrollTo(0, 0);
}

window.addEventListener('popstate', () => {
  route.path = location.pathname;
  route.query = new URLSearchParams(location.search);
});

// Intercept clicks on internal links.
document.addEventListener('click', (e) => {
  const a = (e.target as HTMLElement).closest('a');
  if (!a || a.target || a.hasAttribute('download') || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
  const href = a.getAttribute('href');
  if (!href || !href.startsWith('/') || href.startsWith('/api/') || href.startsWith('/auth/') || a.hasAttribute('data-external')) return;
  e.preventDefault();
  navigate(href);
});
