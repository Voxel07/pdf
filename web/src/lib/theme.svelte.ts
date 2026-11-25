// Light / dark theme. "system" follows the operating system; the choice is kept per browser.
export type ThemeChoice = 'system' | 'light' | 'dark';

const KEY = 'pdfsign_theme';

function stored(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    if (v === 'light' || v === 'dark') return v;
  } catch {}
  return 'system';
}

export const theme = $state<{ choice: ThemeChoice }>({ choice: stored() });

function apply() {
  const root = document.documentElement;
  if (theme.choice === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', theme.choice);
}
apply();

export function setTheme(choice: ThemeChoice) {
  theme.choice = choice;
  try {
    if (choice === 'system') localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {}
  apply();
}

/** Cycles system → dark → light → system. */
export function nextTheme() {
  setTheme(theme.choice === 'system' ? 'dark' : theme.choice === 'dark' ? 'light' : 'system');
}
