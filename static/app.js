(() => {
  if (window.htmx) {
    window.htmx.config.refreshOnHistoryMiss = true;
    window.htmx.config.includeIndicatorStyles = false;
  }
  let themeSwitchTimer = 0;

  const setActiveNav = () => {
    const current = window.location.pathname.replace(/\/$/, '') || '/';
    document.querySelectorAll('.site-nav a[href]').forEach((link) => {
      const target = new URL(link.href, window.location.origin).pathname.replace(/\/$/, '') || '/';
      const active = target === '/' ? current === '/' : current === target || current.startsWith(`${target}/`);
      link.toggleAttribute('aria-current', active);
    });
  };

  const formatRelative = (date) => {
    const seconds = Math.round((Date.now() - date.getTime()) / 1000);
    const future = seconds < 0;
    const amount = Math.abs(seconds);
    let value;
    let unit;
    if (amount < 45) return 'just now';
    if (amount < 90) {
      value = 1;
      unit = 'min';
    } else if (amount < 3600) {
      value = Math.round(amount / 60);
      unit = 'min';
    } else if (amount < 86400) {
      value = Math.round(amount / 3600);
      unit = 'h';
    } else {
      value = Math.round(amount / 86400);
      unit = 'd';
    }
    const text = `${value} ${unit}${value === 1 ? '' : ''}`;
    return future ? `in ${text}` : `${text} ago`;
  };

  const formatTimes = (root = document) => {
    root.querySelectorAll?.('time[data-relative-time]').forEach((time) => {
      const raw = time.getAttribute('datetime') || '';
      const parsed = new Date(raw);
      if (Number.isNaN(parsed.getTime())) return;
      time.title = new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'medium',
      }).format(parsed);
      const nerdRoot = time.closest('[data-nerd-root]');
      time.textContent = nerdRoot?.classList.contains('is-nerd') ? raw : formatRelative(parsed);
    });
    root.querySelectorAll?.('time[data-local-time]').forEach((time) => {
      const raw = time.getAttribute('datetime') || '';
      const parsed = new Date(raw);
      if (Number.isNaN(parsed.getTime())) return;
      time.textContent = new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
      }).format(parsed);
    });
  };
  window.__qedFormatRelative = formatRelative;
  window.__qedFormatTimes = formatTimes;

  const copyText = (value) => {
    if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(value);
    const input = document.createElement('textarea');
    input.value = value;
    input.style.position = 'fixed';
    input.style.opacity = '0';
    document.body.appendChild(input);
    input.select();
    document.execCommand('copy');
    input.remove();
    return Promise.resolve();
  };

  const handleClick = (event) => {
    if (!(event.target instanceof Element)) return;
    const row = event.target.closest('.leaderboard-row[data-detail-url]');
    if (row && !event.target.closest('a')) {
      window.location.href = row.dataset.detailUrl;
      return;
    }
    const toggle = event.target.closest('#theme-toggle');
    if (toggle) {
      const root = document.documentElement;
      const next = root.dataset.theme === 'dark' ? 'light' : 'dark';
      root.dataset.theme = next;
      root.classList.add('theme-switching');
      window.clearTimeout(themeSwitchTimer);
      themeSwitchTimer = window.setTimeout(() => {
        root.classList.remove('theme-switching');
      }, 250);
      localStorage.setItem('qed-theme', next);
      return;
    }
    const nerdToggle = event.target.closest('[data-nerd-toggle]');
    if (nerdToggle) {
      const root = nerdToggle.closest('[data-nerd-root]') || document;
      const panel = root.querySelector('.nerd-panel');
      if (!panel) return;
      const open = panel.hasAttribute('hidden');
      panel.toggleAttribute('hidden', !open);
      if (root instanceof Element) root.classList.toggle('is-nerd', open);
      nerdToggle.setAttribute('aria-expanded', String(open));
      formatTimes(root);
      return;
    }
    const copy = event.target.closest('[data-copy]');
    if (!copy) return;
    event.preventDefault();
    copyText(copy.dataset.copy || '').then(() => {
      const old = copy.textContent;
      copy.textContent = 'Copied';
      window.setTimeout(() => { copy.textContent = old; }, 1200);
    });
  };

  const handleKeydown = (event) => {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    const row = event.target.closest?.('.leaderboard-row[data-detail-url]');
    if (!row || event.target.closest('a')) return;
    event.preventDefault();
    window.location.href = row.dataset.detailUrl;
  };

  const boot = () => {
    const saved = localStorage.getItem('qed-theme');
    if (saved === 'dark' || saved === 'light') document.documentElement.dataset.theme = saved;
    setActiveNav();
    formatTimes();
    const params = new URLSearchParams(window.location.search);
    if (params.get('hero') === 'open') document.documentElement.classList.add('hero-open');
    if (params.get('nerd') === '1') {
      document.querySelectorAll('[data-nerd-root]').forEach((root) => {
        root.classList.add('is-nerd');
        root.querySelectorAll('.nerd-panel').forEach((panel) => panel.removeAttribute('hidden'));
        root.querySelectorAll('[data-nerd-toggle]').forEach((toggle) => toggle.setAttribute('aria-expanded', 'true'));
      });
      formatTimes();
    }
  };

  document.addEventListener('click', handleClick);
  document.addEventListener('keydown', handleKeydown);
  document.addEventListener('DOMContentLoaded', boot, { once: true });
  document.addEventListener('htmx:afterSettle', () => {
    setActiveNav();
    formatTimes();
    window.__qedLeaderboardBoot?.();
  });
  document.addEventListener('htmx:historyRestore', () => {
    setActiveNav();
    formatTimes();
  });
  boot();
})();
