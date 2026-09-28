(() => {
  const picker = document.querySelector('[data-wallet-picker]');
  if (!picker) return;
  const form = document.querySelector('.wallet-form');
  const addressInput = document.querySelector('#wallet-address');
  const error = document.querySelector('[data-wallet-error]');
  const empty = document.querySelector('[data-wallet-empty]');
  const evmProviders = [];
  const solanaWallets = [];
  let render = () => {};

  const identity = (kind, value, fallback) => `${kind}:${value || fallback}`;
  const addEvm = (provider, info = {}) => {
    if (!provider || typeof provider.request !== 'function') return;
    const key = identity('evm', info.rdns, info.name || 'provider');
    if (evmProviders.some((entry) => entry.key === key || entry.provider === provider)) return;
    announced = true;
    evmProviders.push({ key, provider, name: info.name || info.rdns || 'EVM wallet', rdns: info.rdns || '', icon: info.icon || '' });
    render();
  };
  const addSolana = (wallet) => {
    if (!wallet || !wallet.features || typeof wallet.features['standard:connect']?.connect !== 'function') return;
    const name = wallet.name || 'Solana wallet';
    const key = identity('solana', wallet.id, name);
    if (solanaWallets.some((entry) => entry.key === key || entry.wallet === wallet)) return;
    announced = true;
    solanaWallets.push({ key, wallet, name, icon: wallet.icon || '' });
    render();
  };

  window.addEventListener('eip6963:announceProvider', (event) => {
    const detail = event.detail || {};
    addEvm(detail.provider, detail.info || {});
  });
  window.dispatchEvent(new Event('eip6963:requestProvider'));

  const walletRegistry = navigator.wallets;
  if (walletRegistry) {
    try {
      walletRegistry.get?.().forEach(addSolana);
    } catch (_error) {
      // A partially implemented registry should not block paste or legacy wallets.
    }
    try {
      walletRegistry.addEventListener?.('register', (event) => {
        const value = event.detail?.wallet || event.detail;
        Array.isArray(value) ? value.forEach(addSolana) : addSolana(value);
      });
    } catch (_error) {
      // Ignore registries that only expose the global register event.
    }
    try {
      walletRegistry.register?.((wallet) => addSolana(wallet));
    } catch (_error) {
      // Ignore unsupported register signatures.
    }
  }
  window.addEventListener('wallet-standard:register-wallet', (event) => {
    const value = event.detail?.wallet || event.detail;
    Array.isArray(value) ? value.forEach(addSolana) : addSolana(value);
  });

  const legacyFallback = () => {
    if (announced) return;
    addEvm(window.ethereum, { name: 'EVM wallet' });
    addSolana(window.solana);
  };

  const iconNode = (icon) => {
    if (!icon || !icon.startsWith('data:')) return null;
    const image = document.createElement('img');
    image.src = icon;
    image.alt = '';
    image.width = 20;
    image.height = 20;
    return image;
  };
  const walletButton = (entry, kind) => {
    const button = document.createElement('button');
    button.className = 'button secondary-button wallet-choice';
    button.type = 'button';
    const icon = iconNode(entry.icon);
    if (icon) button.append(icon);
    const label = document.createElement('span');
    label.textContent = entry.name;
    button.append(label);
    if (kind === 'evm' && entry.rdns) button.title = entry.rdns;
    button.addEventListener('click', () => connect(entry, kind, button));
    return button;
  };
  render = () => {
    picker.replaceChildren();
    evmProviders.forEach((entry) => picker.append(walletButton(entry, 'evm')));
    solanaWallets.forEach((entry) => picker.append(walletButton(entry, 'solana')));
    const hasWallet = evmProviders.length > 0 || solanaWallets.length > 0;
    picker.hidden = !hasWallet;
    if (empty) empty.hidden = hasWallet;
  };
  const connect = async (entry, kind, button) => {
    button.disabled = true;
    if (error) error.textContent = '';
    try {
      let address;
      if (kind === 'evm') {
        const accounts = await entry.provider.request({ method: 'eth_requestAccounts' });
        address = accounts && accounts[0];
      } else {
        const response = await entry.wallet.features['standard:connect'].connect();
        address = response?.accounts?.[0]?.address;
      }
      if (address && form && addressInput) {
        addressInput.value = address;
        form.requestSubmit();
      } else if (error) {
        error.textContent = 'No wallet address was returned.';
      }
    } catch (_error) {
      if (error) error.textContent = 'Wallet connection was cancelled.';
    } finally {
      button.disabled = false;
    }
  };

  render();
  setTimeout(() => {
    legacyFallback();
    render();
  }, 150);
})();
