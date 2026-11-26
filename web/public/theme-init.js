// Applies the theme chosen in the admin UI before the first paint (see src/lib/theme.svelte.ts).
// A separate file so the Content-Security-Policy can forbid inline scripts.
try {
  var t = localStorage.getItem('pdfsign_theme');
  if (t === 'light' || t === 'dark') document.documentElement.setAttribute('data-theme', t);
} catch (e) {}
