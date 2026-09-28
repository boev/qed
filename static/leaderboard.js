(() => {
  let activeRoot;
  let activeApp;

  const boot = () => {
    const root = document.getElementById('leaderboard');
    if (!root || !window.Vue) {
      if (!root && activeApp) {
        activeApp.unmount();
        activeApp = null;
        activeRoot = null;
      }
      return;
    }
    if (root === activeRoot) return;
    if (activeApp) {
      activeApp.unmount();
      activeApp = null;
      activeRoot = null;
    }

    const initial = (() => {
      const script = document.getElementById('leaderboard-data');
      if (!script) return null;
      try {
        return JSON.parse(script.textContent || '{}');
      } catch (_error) {
        return null;
      }
    })();

    const { computed, onBeforeUnmount, onMounted, onUpdated, ref } = Vue;
    const fullRefreshInterval = 60_000;
    const perPage = 50;
    const sortFields = {
      price: 'price_usd',
      change: 'change_24h_pct',
      volume: 'volume_24h_usd',
      liquidity: 'liquidity_usd',
    };
    const sortKeys = new Set(Object.keys(sortFields));
    const priceFields = Object.values(sortFields);
    const venueLabels = Object.freeze({
      meteora: 'Meteora',
      orca: 'Orca',
      pancakeswap: 'PancakeSwap',
      pumpswap: 'PumpSwap',
      ramses: 'Ramses',
      raydium: 'Raydium',
      uniswap: 'Uniswap',
    });

    const app = Vue.createApp({
      setup() {
        const initialPayload = initial || { entries: [] };
        const initialEntries = Array.isArray(initialPayload.entries)
          ? initialPayload.entries
          : [];
        const initialPer = Number(initialPayload.per) || perPage;
        const queryPage = Number.parseInt(
          new URLSearchParams(window.location.search).get('page') || '',
          10,
        );
        const initialPage = Number.isInteger(queryPage) && queryPage > 0
          ? queryPage
          : Number(initialPayload.page) || 1;
        const initialStart = (initialPage - 1) * initialPer;
        const payload = ref({
          ...initialPayload,
          entries: initialEntries.slice(initialStart, initialStart + initialPer),
          page: initialPage,
          per: initialPer,
          total: typeof initialPayload.total === 'number' && Number.isFinite(initialPayload.total)
            ? initialPayload.total
            : initialEntries.length,
        });
        const currentPage = ref(initialPage);
        const sortKey = ref('volume');
        const descending = ref(true);
        const flashCells = ref(new Map());
        let fullRefreshTimer;
        let flashTimer;
        let statusTimer;
        let requestNumber = 0;
        const statusTick = ref(Date.now());
        const pricesUpdatedAt = ref(initialPayload.prices_updated_at || '');
        const numberValue = (value) => {
          if (value === null || value === undefined || value === '') return null;
          const number = typeof value === 'number' ? value : Number(value);
          return Number.isFinite(number) ? number : null;
        };

        const entries = computed(() =>
          Array.isArray(payload.value.entries) ? payload.value.entries : []);

        const totalPages = computed(() => Math.max(1,
          Math.ceil((Number(payload.value.total) || 0) / (Number(payload.value.per) || perPage))));
        const pageNumbers = computed(() => Array.from(
          { length: totalPages.value },
          (_value, index) => index + 1,
        ));

        const scheduleFlashClear = () => {
          window.clearTimeout(flashTimer);
          flashTimer = window.setTimeout(() => {
            flashCells.value = new Map();
          }, 900);
        };

        const markChanged = (key, before, after, changed) => {
          const oldValue = numberValue(before);
          const newValue = numberValue(after);
          if (oldValue === null || newValue === null || oldValue === newValue) return;
          changed.set(key, newValue > oldValue ? 'up' : 'down');
        };

        const replacePayload = (next) => {
          if (!next || !Array.isArray(next.entries)) return false;
          const previous = new Map((payload.value.entries || []).map((row) => [
            `${row.chain}:${row.pool}`,
            row,
          ]));
          const changed = new Map(flashCells.value);
          next.entries.forEach((row) => {
            const before = previous.get(`${row.chain}:${row.pool}`);
            if (!before) return;
            priceFields.forEach((field) => markChanged(
              `${row.chain}:${row.pool}:${field}`,
              before[field],
              row[field],
              changed,
            ));
          });
          const nextPer = Number(next.per) || perPage;
          const nextPage = Number(next.page) || currentPage.value;
          const nextTotal = typeof next.total === 'number' && Number.isFinite(next.total)
            ? next.total
            : next.entries.length;
          payload.value = {
            ...next,
            page: nextPage,
            per: nextPer,
            total: nextTotal,
          };
          if (next.prices_updated_at) pricesUpdatedAt.value = next.prices_updated_at;
          currentPage.value = nextPage;
          if (changed.size) {
            flashCells.value = changed;
            scheduleFlashClear();
          }
          return true;
        };

        const fetchLeaderboard = async () => {
          const request = ++requestNumber;
          const params = new URLSearchParams({
            page: String(currentPage.value),
            per: String(perPage),
            sort: sortKey.value,
            dir: descending.value ? 'desc' : 'asc',
          });
          try {
            const response = await fetch(`/api/leaderboard?${params.toString()}`, {
              headers: { Accept: 'application/json' },
            });
            if (!response.ok || request !== requestNumber) return;
            replacePayload(await response.json());
          } catch (_error) {
            // Keep the last complete snapshot visible when a refresh is unavailable.
          }
        };


        const updateSort = (key) => {
          if (!sortKeys.has(key)) return;
          if (sortKey.value === key) {
            descending.value = !descending.value;
          } else {
            sortKey.value = key;
            descending.value = true;
          }
          currentPage.value = 1;
          fetchLeaderboard();
        };

        const goToPage = (nextPage) => {
          const target = Math.max(1, Math.min(totalPages.value, nextPage));
          if (target === currentPage.value) return;
          currentPage.value = target;
          fetchLeaderboard();
        };

        const numberLabel = (value, prefix = '') => {
          const number = numberValue(value);
          if (number === null) return '—';
          const absolute = Math.abs(number);
          const maximumFractionDigits = absolute && absolute < 1 ? 6 : 2;
          return `${prefix}${new Intl.NumberFormat('en-US', {
            maximumFractionDigits,
            notation: absolute >= 1_000_000 ? 'compact' : 'standard',
            compactDisplay: 'short',
          }).format(number)}`;
        };

        const percentLabel = (value) => {
          const number = numberValue(value);
          if (number === null) return '—';
          return `${number >= 0 ? '+' : ''}${number.toFixed(2)}%`;
        };

        const statusLabel = computed(() => {
          statusTick.value;
          const relative = window.__qedFormatRelative || (() => '—');
          const updatedDate = new Date(payload.value.updated_at || '');
          const nextDate = new Date(payload.value.next_refresh_at || '');
          const priceDate = new Date(pricesUpdatedAt.value || '');
          const hasEntries = Array.isArray(payload.value.entries) && payload.value.entries.length > 0;
          const emptyState = payload.value.refreshing
            ? 'Updating…'
            : !hasEntries && !payload.value.empty_successful
              ? 'Building the first board…'
              : null;
          const nextText = payload.value.refreshing
            || Number.isNaN(nextDate.getTime())
            || nextDate.getTime() <= statusTick.value
            ? 'updating now'
            : relative(nextDate);
          const updatedText = Number.isNaN(updatedDate.getTime())
            ? '—'
            : `${relative(updatedDate)}${Date.now() - updatedDate.getTime() >= 3_600_000 ? ' · stale' : ''}`;
          return {
            emptyState,
            updatedText,
            nextText,
            pricesText: Number.isNaN(priceDate.getTime()) ? '—' : relative(priceDate),
          };
        });

        const sealIcon = (verdict) => {
          if (verdict === 'verified') return 'qed-seal-verified';
          if (verdict === 'mismatch' || verdict === 'nomatch') return 'qed-seal-broken';
          return 'qed-seal-unknown';
        };
        const chainIcon = (chain) => chain === 'robinhoodchain' ? 'robinhood' : chain;
        const venueLabel = (venue) => {
          const normalized = String(venue || '').toLowerCase();
          return venueLabels[normalized] || venue || '—';
        };

        const verdictLabel = (verdict) => {
          if (verdict === 'verified') return 'Verified';
          if (verdict === 'mismatch') return 'Mismatch';
          if (verdict === 'nomatch') return 'No match';
          return 'Unknown';
        };

        const flashClass = (row, field) => {
          const direction = flashCells.value.get(`${row.chain}:${row.pool}:${field}`);
          return direction ? `is-flash-${direction}` : '';
        };

        onMounted(() => {
          fetchLeaderboard();
          fullRefreshTimer = window.setInterval(fetchLeaderboard, fullRefreshInterval);
          statusTimer = window.setInterval(() => {
            statusTick.value = Date.now();
          }, 1_000);
        });
        onUpdated(() => {
          window.__qedFormatTimes?.();
        });
        onBeforeUnmount(() => {
          window.clearInterval(fullRefreshTimer);
          window.clearInterval(statusTimer);
          window.clearTimeout(flashTimer);
        });

        return () => {
          const h = Vue.h;
          const sortButton = (key, label) => h('button', {
            type: 'button',
            class: ['sort-button', { 'is-sorted': sortKey.value === key }],
            onClick: () => updateSort(key),
            'aria-label': `Sort by ${label}`,
          }, [label, h('span', { 'aria-hidden': 'true' },
            sortKey.value === key ? (descending.value ? ' ↓' : ' ↑') : ' ↕')]);
          const header = h('thead', {}, [
            h('tr', {}, [
              h('th', { scope: 'col', class: 'rank-cell' }, 'Rank'),
              h('th', { scope: 'col' }, 'Pair'),
              h('th', { scope: 'col' }, 'Chain'),
              h('th', { scope: 'col' }, 'Venue'),
              h('th', { scope: 'col' }, 'Verdict'),
              h('th', { scope: 'col' }, [sortButton('price', 'Price')]),
              h('th', { scope: 'col' }, [sortButton('change', '24h change')]),
              h('th', { scope: 'col' }, [sortButton('volume', '24h volume')]),
              h('th', { scope: 'col' }, [sortButton('liquidity', 'Liquidity')]),
              h('th', { scope: 'col', class: 'trade-heading' }, [
                h('span', { class: 'visually-hidden' }, 'Trade'),
              ]),
            ]),
          ]);
          const metricCell = (row, field, label, value, prefix = '') => h('td', {
            class: ['number-cell', 'metric-cell', flashClass(row, field)],
            'data-label': label,
          }, [h('span', { class: 'metric-value' }, numberLabel(value, prefix))]);
          const rows = entries.value.map((row) => {
            const change = numberValue(row.change_24h_pct);
            const detailUrl = row.detail_url || `/validated/${row.chain}/${row.pool}`;
            const tradeUrl = row.trade_url || '';
            const changeLabel = `${change !== null && change > 0 ? '↑ ' : change !== null && change < 0 ? '↓ ' : ''}${percentLabel(change)}`;
            return h('tr', {
              key: `${row.chain}:${row.pool}`,
              class: 'leaderboard-row',
              tabindex: '0',
              role: 'link',
              'aria-label': `${row.base_symbol || 'Unknown'} / ${row.quote_symbol || 'Unknown'}`,
              'data-detail-url': detailUrl,
              onClick: (event) => {
                if (!event.target.closest('a')) window.location.href = detailUrl;
              },
              onKeydown: (event) => {
                if ((event.key === 'Enter' || event.key === ' ') && !event.target.closest('a')) {
                  event.preventDefault();
                  window.location.href = detailUrl;
                }
              },
            }, [
              h('td', { class: 'rank-cell', 'data-label': 'Rank' }, row.rank),
              h('td', { class: 'token-cell', 'data-label': 'Pair' }, [
                h('a', { class: 'leaderboard-row-link', href: detailUrl }, [
                  h('span', { class: 'token-pair' }, [
                    h('strong', {}, row.base_symbol || 'Unknown'),
                    h('span', {}, `/ ${row.quote_symbol || 'Unknown'}`),
                  ]),
                ]),
              ]),
              h('td', { class: 'chain-cell', 'data-label': 'Chain' }, [
                h('span', { class: 'chain-label' }, [
                  h('svg', { 'aria-hidden': 'true' }, [
                    h('use', { href: `/static/icons.svg#${chainIcon(row.chain)}` }),
                  ]),
                  h('span', {}, row.chain_label || row.chain),
                ]),
              ]),
              h('td', { class: 'venue-cell', 'data-label': 'Venue' }, venueLabel(row.dex)),
              h('td', { 'data-label': 'Verdict' }, [
                h('span', {
                  class: ['seal-label', `is-${row.verdict || 'unknown'}`],
                  title: row.verdict === 'verified'
                    ? 'contract match only, not custody, price or endorsement'
                    : undefined,
                }, [
                  h('svg', { class: 'seal-icon', 'aria-hidden': 'true' }, [
                    h('use', { href: `/static/icons.svg#${sealIcon(row.verdict)}` }),
                  ]),
                  h('span', {}, verdictLabel(row.verdict)),
                ]),
              ]),
              metricCell(row, 'price_usd', 'Price', row.price_usd, '$'),
              h('td', {
                class: ['number-cell', 'metric-cell', 'change-cell', {
                  'is-positive': change !== null && change > 0,
                  'is-negative': change !== null && change < 0,
                  [flashClass(row, 'change_24h_pct')]: Boolean(flashClass(row, 'change_24h_pct')),
                }],
                'data-label': '24h change',
              }, [h('span', { class: 'metric-value' }, changeLabel)]),
              metricCell(row, 'volume_24h_usd', '24h volume', row.volume_24h_usd, '$'),
              metricCell(row, 'liquidity_usd', 'Liquidity', row.liquidity_usd, '$'),
              h('td', { class: 'trade-cell', 'data-label': 'Trade' }, tradeUrl ? [
                h('a', {
                  href: tradeUrl,
                  title: row.verdict === 'verified'
                    ? 'contract match only, not custody, price or endorsement'
                    : undefined,
                  target: '_blank',
                  rel: 'noopener noreferrer',
                  onClick: (event) => event.stopPropagation(),
                }, ['Trade', h('span', { 'aria-hidden': 'true' }, ' ↗')]),
              ] : '—'),
            ]);
          });
          if (!rows.length && payload.value.empty_successful) {
            rows.push(h('tr', {}, [
              h('td', { colspan: '10', class: 'leaderboard-empty' },
                'No stock-paired pools are available right now.'),
            ]));
          }
          const pagination = h('nav', {
            class: 'leaderboard-pagination',
            'aria-label': 'Leaderboard pages',
          }, [
            h('button', {
              type: 'button',
              disabled: currentPage.value <= 1,
              onClick: () => goToPage(currentPage.value - 1),
              'aria-label': 'Previous page',
            }, 'Previous'),
            ...pageNumbers.value.map((page) => h('button', {
              type: 'button',
              'aria-current': page === currentPage.value ? 'page' : undefined,
              onClick: () => goToPage(page),
            }, String(page))),
            h('button', {
              type: 'button',
              disabled: currentPage.value >= totalPages.value,
              onClick: () => goToPage(currentPage.value + 1),
              'aria-label': 'Next page',
            }, 'Next'),
          ]);
          return h('div', {}, [
            h('div', { class: 'leaderboard-table-wrap' }, [
              h('table', { class: 'leaderboard-table stack-table' }, [
                h('caption', { class: 'visually-hidden' }, 'Live stock-paired token leaderboard'),
                header,
                h('tbody', {}, rows),
              ]),
            ]),
            h('p', { class: 'contract-match-footnote' },
              'Verified means contract match only: not custody, price or endorsement.'),
            h('p', { class: 'leaderboard-status', 'aria-live': 'polite' },
              statusLabel.value.emptyState || [
                'Prices and volume from DexScreener, prices ',
                statusLabel.value.pricesText,
                ' · updated ',
                statusLabel.value.updatedText,
                ' · next ',
                statusLabel.value.nextText,
                '. Verdicts read from chain.',
              ]),
            pagination,
          ]);
        };
      },
    });

    app.mount(root);
    activeRoot = root;
    activeApp = app;
  };

  window.__qedLeaderboardBoot = boot;
  boot();
})();
