(() => {
  if (window.htmx) {
    window.htmx.config.refreshOnHistoryMiss = true;
    window.htmx.config.includeIndicatorStyles = false;
  }
  let themeSwitchTimer = 0;

  const setActiveNav = () => {
    const current = window.location.pathname.replace(/\/$/, '') || '/';
    const links = [...document.querySelectorAll('[data-main-nav] a[href]')];
    const targetOf = (link) => new URL(link.href, window.location.origin).pathname.replace(/\/$/, '') || '/';
    const matches = (target) => (target === '/' ? current === '/' : current === target || current.startsWith(`${target}/`));
    // Only the most specific match is current, so /docs/llm marks "MCP setup", not also "Documentation".
    const best = links.map(targetOf).filter(matches).reduce((longest, target) => (target.length > longest.length ? target : longest), '');
    links.forEach((link) => {
      if (best && targetOf(link) === best) link.setAttribute('aria-current', 'page');
      else link.removeAttribute('aria-current');
    });
  };


  const formatTimes = (root = document) => {
    root.querySelectorAll?.('time[datetime]').forEach((time) => {
      const raw = time.getAttribute('datetime') || '';
      if (raw) time.title = raw;
    });
  };
  window.__qedFormatTimes = formatTimes;
  let registryTickers = null;
  let registryTickersPromise = null;
  const isLookupTicker = (ticker) => typeof ticker === 'string'
    && ticker.length > 0
    && ticker.length <= 32
    && /^[A-Za-z0-9._-]+$/.test(ticker);
  const closeTickerSuggestions = (input) => {
    const list = document.getElementById('ticker-suggestions');
    if (!list) return;
    list.hidden = true;
    list.replaceChildren();
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
  };
  const updateTickerSuggestions = (input) => {
    const list = document.getElementById('ticker-suggestions');
    if (!list || registryTickers === null) return;
    if (document.activeElement !== input) {
      closeTickerSuggestions(input);
      return;
    }
    const query = input.value.trim().toLowerCase();
    const matches = query
      ? registryTickers.filter((ticker) => ticker.toLowerCase().startsWith(query)).slice(0, 8)
      : [];
    list.replaceChildren(...matches.map((ticker, index) => {
      const option = document.createElement('div');
      option.id = `ticker-suggestion-${index}`;
      option.className = 'ticker-suggestion';
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', 'false');
      option.dataset.ticker = ticker;
      option.textContent = ticker;
      return option;
    }));
    list.hidden = matches.length === 0;
    input.setAttribute('aria-expanded', String(matches.length > 0));
    input.removeAttribute('aria-activedescendant');
  };
  const selectTickerSuggestion = (input, ticker) => {
    input.value = ticker;
    closeTickerSuggestions(input);
    input.focus();
  };
  const loadTickerSuggestions = () => {
    if (!registryTickersPromise) {
      registryTickersPromise = fetch('/api/registry', { headers: { Accept: 'application/json' } })
        .then((response) => {
          if (!response.ok) throw new Error('Registry suggestions unavailable');
          return response.json();
        })
        .then((entries) => {
          if (!Array.isArray(entries)) throw new Error('Registry suggestions unavailable');
          const seen = new Set();
          registryTickers = [];
          entries.forEach((entry) => {
            if (!entry || entry.removed_at != null || entry.stale_since != null) return;
            const ticker = entry.ticker;
            if (!isLookupTicker(ticker)) return;
            const key = ticker.toLowerCase();
            if (seen.has(key)) return;
            seen.add(key);
            registryTickers.push(ticker);
          });
        })
        .catch(() => {
          registryTickers = [];
        });
    }
    return registryTickersPromise.then(() => {
      const input = document.getElementById('ticker');
      if (input) updateTickerSuggestions(input);
    });
  };
  const initTickerLookup = () => {
    if (registryTickers !== null) {
      const input = document.getElementById('ticker');
      if (input) updateTickerSuggestions(input);
    }
  };

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
    const printStatement = event.target.closest('[data-print-statement]');
    if (printStatement) {
      window.print();
      return;
    }
    const suggestion = event.target.closest('.ticker-suggestion');
    if (suggestion) {
      event.preventDefault();
      const input = document.getElementById('ticker');
      if (input) selectTickerSuggestion(input, suggestion.dataset.ticker);
      return;
    }
    const tickerInput = document.getElementById('ticker');
    if (tickerInput && !event.target.closest('.ticker-combobox')) closeTickerSuggestions(tickerInput);
    const row = event.target.closest('.leaderboard-row[data-detail-url]');
    if (row && row.dataset.vueRow !== 'true' && !event.target.closest('a')) {
      window.location.href = row.dataset.detailUrl;
      return;
    }
    const toggle = event.target.closest('[data-theme-toggle]');
    if (toggle) {
      const root = document.documentElement;
      const followsDarkSystem =
        root.dataset.theme === 'auto' && window.matchMedia('(prefers-color-scheme: dark)').matches;
      const isDark = root.dataset.theme === 'dark' || followsDarkSystem;
      const next = isDark ? 'light' : 'dark';
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
      const oldLabel = copy.dataset.copyOriginalLabel
        || copy.getAttribute('aria-label')
        || copy.textContent;
      const oldText = copy.dataset.copyOriginalText || copy.textContent;
      const oldTitle = copy.dataset.copyOriginalTitle || copy.getAttribute('title') || '';
      copy.dataset.copyOriginalLabel = oldLabel || '';
      copy.dataset.copyOriginalText = oldText || '';
      copy.dataset.copyOriginalTitle = oldTitle;
      copy.setAttribute('aria-label', 'Copied');
      copy.setAttribute('title', 'Copied');
      const icon = copy.querySelector('use');
      if (icon) {
        copy.classList.add('is-copied');
        icon.setAttribute('href', '/static/icons.svg#check');
      } else {
        copy.textContent = 'Copied';
      }
      const status = document.getElementById('copy-status');
      if (status) status.textContent = 'Copied';
      window.clearTimeout(Number(copy.dataset.copyFeedbackTimer));
      copy.dataset.copyFeedbackTimer = String(window.setTimeout(() => {
        if (oldLabel) copy.setAttribute('aria-label', oldLabel);
        else copy.removeAttribute('aria-label');
        if (oldTitle) copy.setAttribute('title', oldTitle);
        else copy.removeAttribute('title');
        if (icon) {
          copy.classList.remove('is-copied');
          icon.setAttribute('href', '/static/icons.svg#copy');
        } else {
          copy.textContent = oldText;
        }
        if (status) status.textContent = '';
        delete copy.dataset.copyFeedbackTimer;
      }, 1200));
    });
  };

  const handleKeydown = (event) => {
    const input = event.target instanceof HTMLInputElement && event.target.id === 'ticker'
      ? event.target : null;
    if (input) {
      const list = document.getElementById('ticker-suggestions');
      const options = list ? Array.from(list.querySelectorAll('[role="option"]')) : [];
      const activeIndex = options.findIndex((option) => option.getAttribute('aria-selected') === 'true');
      if ((event.key === 'ArrowDown' || event.key === 'ArrowUp') && options.length > 0) {
        event.preventDefault();
        const direction = event.key === 'ArrowDown' ? 1 : -1;
        const nextIndex = activeIndex < 0
          ? (direction > 0 ? 0 : options.length - 1)
          : (activeIndex + direction + options.length) % options.length;
        options.forEach((option, index) => option.setAttribute('aria-selected', String(index === nextIndex)));
        input.setAttribute('aria-activedescendant', options[nextIndex].id);
        options[nextIndex].scrollIntoView({ block: 'nearest' });
        return;
      }
      if (event.key === 'Escape' && list && !list.hidden) {
        event.preventDefault();
        closeTickerSuggestions(input);
        return;
      }
      if (event.key === 'Enter' && activeIndex >= 0) {
        event.preventDefault();
        selectTickerSuggestion(input, options[activeIndex].dataset.ticker);
        return;
      }
    }
    if (event.key !== 'Enter' && event.key !== ' ') return;
    const target = event.target instanceof Element ? event.target : null;
    const row = target?.closest('.leaderboard-row[data-detail-url]');
    if (!row || row.dataset.vueRow === 'true' || target.closest('a')) return;
    event.preventDefault();
    window.location.href = row.dataset.detailUrl;
  };

  const initIntegrationMarquee = () => {
    const track = document.querySelector('.integration-track');
    const firstSet = track?.querySelector('.integration-set:not(.integration-set--clone)');
    if (!track || !firstSet) return;
    const width = firstSet.getBoundingClientRect().width;
    if (width <= 0) return;
    for (const sheet of document.styleSheets) {
      if (!sheet.href || new URL(sheet.href, document.baseURI).origin !== window.location.origin) continue;
      let rules;
      try {
        rules = sheet.cssRules;
      } catch (_error) {
        continue;
      }
      for (const rule of rules) {
        if (rule.selectorText !== '.integration-track') continue;
        rule.style.setProperty('--marquee-duration', `${width / 25}s`);
        return;
      }
    }
  };
  const updateAddressHint = (input, forceError = false) => {
    const hint = document.getElementById(input.dataset.addressHint || '');
    if (!hint) return;
    hint.textContent = forceError || (input.value && !input.validity.valid)
      ? input.dataset.validationError
      : input.dataset.validationHint;
  };
  const initAddressHints = (root = document) => {
    root.querySelectorAll?.('[data-address-hint]').forEach((input) => updateAddressHint(input));
  };
  window.addEventListener('resize', initIntegrationMarquee, { passive: true });

  const boot = () => {
    const saved = localStorage.getItem('qed-theme');
    if (saved === 'dark' || saved === 'light') document.documentElement.dataset.theme = saved;
    setActiveNav();
    formatTimes();
    initTickerLookup();
    initIntegrationMarquee();
    initAddressHints();
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
  document.addEventListener('focusin', (event) => {
    if (event.target instanceof HTMLInputElement && event.target.id === 'ticker') {
      void loadTickerSuggestions();
    }
  });
  document.addEventListener('input', (event) => {
    if (event.target instanceof HTMLInputElement && event.target.id === 'ticker') {
      void loadTickerSuggestions();
    }
    if (event.target instanceof HTMLInputElement && event.target.hasAttribute('data-address-hint')) {
      updateAddressHint(event.target);
    }
  });
  document.addEventListener('invalid', (event) => {
    if (event.target instanceof HTMLInputElement && event.target.hasAttribute('data-address-hint')) {
      event.target.closest('details')?.setAttribute('open', '');
      updateAddressHint(event.target, true);
    }
  }, true);
  document.addEventListener('pointerdown', (event) => {
    if (event.target instanceof Element && event.target.closest('.ticker-suggestion')) {
      event.preventDefault();
    }
  });
  document.addEventListener('focusout', (event) => {
    if (event.target instanceof HTMLInputElement && event.target.id === 'ticker') {
      window.setTimeout(() => {
        if (!event.target.matches(':focus')) closeTickerSuggestions(event.target);
      }, 0);
    }
  });
  const navMenus = () => document.querySelectorAll('details.docs-menu[open], details.mobile-nav-menu[open]');
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    navMenus().forEach((menu) => {
      menu.open = false;
      menu.querySelector('summary')?.focus();
    });
  });
  document.addEventListener('click', (event) => {
    navMenus().forEach((menu) => {
      if (!menu.contains(event.target)) menu.open = false;
    });
  });
  document.addEventListener('DOMContentLoaded', boot, { once: true });
  document.addEventListener('htmx:afterSettle', () => {
    setActiveNav();
    formatTimes();
    initTickerLookup();
    initIntegrationMarquee();
    initAddressHints();
    window.__qedLeaderboardBoot?.();
  });
  document.addEventListener('htmx:historyRestore', () => {
    setActiveNav();
    formatTimes();
    initTickerLookup();
    initIntegrationMarquee();
    initAddressHints();
    window.__qedLeaderboardBoot?.();
  });
  boot();
})();
