(() => {
  let activeHost = null;
  let cleanup = null;
  const stop = () => {
    if (cleanup) cleanup();
    cleanup = null;
    activeHost = null;
  };
  const start = () => {
    const host = document.querySelector('.hex-bg');
    if (!host) {
      stop();
      return;
    }
    if (host === activeHost && host.isConnected) return;
    stop();

  const R = 34;
  const SPEED = 70;
  const TAIL = 140;
  const MAX_SIGNALS = 10;
  const FRAME_INTERVAL = 1000 / 30;
  const root = document.documentElement;
  const motionPreference = window.matchMedia('(prefers-reduced-motion: reduce)');
  const systemTheme = window.matchMedia('(prefers-color-scheme: dark)');
  let reducedMotion = motionPreference.matches;
  let dark = false;
  let width = 0;
  let height = 0;
  let dpr = 1;
  let vertices = [];
  let adjacency = [];
  let signals = [];
  let frameId = 0;
  let lastFrame = 0;

  const canvas = document.createElement('canvas');
  canvas.className = 'hex-comets-canvas';
  const context = canvas.getContext('2d');
  const lattice = document.createElement('canvas');
  const latticeContext = lattice.getContext('2d');
  if (!context || !latticeContext) return;
  host.querySelectorAll('canvas.hex-comets-canvas').forEach((staleCanvas) => staleCanvas.remove());
  host.appendChild(canvas);

  const isDarkTheme = () =>
    root.dataset.theme === 'dark'
    || (root.dataset.theme === 'auto' && systemTheme.matches);
  const fade = (y) => Math.max(0, Math.min(1, (y / height - 0.12) / 0.7)) ** 1.2;

  function build() {
    dpr = Math.min(2, window.devicePixelRatio || 1);
    width = window.innerWidth;
    height = window.innerHeight;
    for (const target of [canvas, lattice]) {
      target.width = Math.round(width * dpr);
      target.height = Math.round(height * dpr);
    }
    context.setTransform(dpr, 0, 0, dpr, 0, 0);
    latticeContext.setTransform(dpr, 0, 0, dpr, 0, 0);

    const index = new Map();
    vertices = [];
    adjacency = [];
    const vertexId = (x, y) => {
      const key = `${Math.round(x * 2)}:${Math.round(y * 2)}`;
      if (!index.has(key)) {
        index.set(key, vertices.length);
        vertices.push([x, y]);
        adjacency.push(new Set());
      }
      return index.get(key);
    };
    const gridWidth = Math.sqrt(3) * R;
    const gridHeight = 1.5 * R;
    for (let row = -1; row * gridHeight < height + 2 * R; row += 1) {
      for (let col = -1; col * gridWidth < width + 2 * R; col += 1) {
        const centerX = col * gridWidth + (row & 1 ? gridWidth / 2 : 0);
        const centerY = row * gridHeight;
        const ids = [];
        for (let corner = 0; corner < 6; corner += 1) {
          const angle = Math.PI / 180 * (60 * corner - 30);
          ids.push(vertexId(centerX + R * Math.cos(angle), centerY + R * Math.sin(angle)));
        }
        for (let edge = 0; edge < 6; edge += 1) {
          const first = ids[edge];
          const second = ids[(edge + 1) % 6];
          adjacency[first].add(second);
          adjacency[second].add(first);
        }
      }
    }
    adjacency = adjacency.map((neighbors) => [...neighbors]);

    latticeContext.clearRect(0, 0, width, height);
    latticeContext.lineWidth = 1;
    const latticeAlpha = dark ? 0.2 : 0.17;
    vertices.forEach(([x1, y1], first) => {
      adjacency[first].forEach((second) => {
        if (second < first) return;
        const [x2, y2] = vertices[second];
        const alpha = latticeAlpha * fade((y1 + y2) / 2);
        if (alpha < 0.004) return;
        latticeContext.strokeStyle = `rgba(153, 69, 255, ${alpha})`;
        latticeContext.beginPath();
        latticeContext.moveTo(x1, y1);
        latticeContext.lineTo(x2, y2);
        latticeContext.stroke();
      });
    });
    signals = [];
  }

  function spawn() {
    let vertex;
    do {
      vertex = Math.floor(Math.random() * vertices.length);
    } while (fade(vertices[vertex][1]) < 0.35);
    const neighbors = adjacency[vertex];
    const next = neighbors[Math.floor(Math.random() * neighbors.length)];
    signals.push({
      path: [vertex, next],
      progress: 0,
      life: 6 + Math.random() * 6,
      age: 0,
      phase: Math.random() * 6.28,
    });
  }

  function step(signal, distance) {
    signal.progress += distance / R;
    while (signal.progress >= 1) {
      signal.progress -= 1;
      const current = signal.path[signal.path.length - 1];
      const previous = signal.path[signal.path.length - 2];
      const options = adjacency[current].filter((neighbor) => neighbor !== previous);
      signal.path.push(options[Math.floor(Math.random() * options.length)]);
      if (signal.path.length > 12) signal.path.shift();
    }
  }

  function slice(points, length) {
    const result = [points[0]];
    for (let index = 0; index < points.length - 1 && length > 0; index += 1) {
      const [x1, y1] = points[index];
      const [x2, y2] = points[index + 1];
      const distance = Math.hypot(x2 - x1, y2 - y1);
      if (distance >= length) {
        const fraction = length / distance;
        result.push([x1 + (x2 - x1) * fraction, y1 + (y2 - y1) * fraction]);
        break;
      }
      result.push(points[index + 1]);
      length -= distance;
    }
    return result;
  }

  function drawSignal(signal, now) {
    const path = signal.path;
    const end = path.length;
    const [startX, startY] = vertices[path[end - 2]];
    const [endX, endY] = vertices[path[end - 1]];
    const points = [[
      startX + (endX - startX) * signal.progress,
      startY + (endY - startY) * signal.progress,
    ]];
    let remaining = TAIL - signal.progress * R;
    points.push([startX, startY]);
    for (let index = end - 2; index > 0 && remaining > 0; index -= 1) {
      const [x1, y1] = vertices[path[index]];
      const [x0, y0] = vertices[path[index - 1]];
      const fraction = Math.min(1, remaining / R);
      points.push([x1 + (x0 - x1) * fraction, y1 + (y0 - y1) * fraction]);
      remaining -= R;
    }
    const life = Math.min(1, signal.age, signal.life - signal.age);
    const flicker = 0.92 + 0.08 * Math.sin(now / 60 + signal.phase);
    const [headX, headY] = points[0];
    const glow = life * flicker * fade(headY);
    if (glow < 0.01) return;

    context.globalCompositeOperation = dark ? 'lighter' : 'source-over';
    context.lineCap = 'round';
    context.lineJoin = 'round';
    for (let pass = 1; pass <= 12; pass += 1) {
      const fraction = pass / 12;
      const segment = slice(points, TAIL * fraction);
      const near = 1 - fraction;
      context.lineWidth = 0.9 + 6.5 * near * near;
      const alpha = (dark ? 0.085 : 0.07) * glow;
      const red = dark ? Math.round(153 + 60 * near) : 153;
      const green = dark ? Math.round(69 + 110 * near) : 69;
      context.strokeStyle = `rgba(${red}, ${green}, 255, ${alpha})`;
      context.beginPath();
      context.moveTo(segment[0][0], segment[0][1]);
      for (let index = 1; index < segment.length; index += 1) {
        context.lineTo(segment[index][0], segment[index][1]);
      }
      context.stroke();
    }

    const core = slice(points, TAIL * 0.35);
    context.lineWidth = 1.2;
    context.strokeStyle = dark
      ? `rgba(240, 225, 255, ${0.55 * glow})`
      : `rgba(153, 69, 255, ${0.45 * glow})`;
    context.beginPath();
    context.moveTo(core[0][0], core[0][1]);
    for (let index = 1; index < core.length; index += 1) {
      context.lineTo(core[index][0], core[index][1]);
    }
    context.stroke();

    const halo = 10;
    const gradient = context.createRadialGradient(headX, headY, 0, headX, headY, halo);
    const coreAlpha = (dark ? 0.7 : 0.45) * glow;
    gradient.addColorStop(0, dark
      ? `rgba(255, 250, 255, ${coreAlpha})`
      : `rgba(153, 69, 255, ${coreAlpha})`);
    gradient.addColorStop(0.35, `rgba(170, 100, 255, ${coreAlpha * 0.35})`);
    gradient.addColorStop(1, 'rgba(153, 69, 255, 0)');
    context.fillStyle = gradient;
    context.beginPath();
    context.arc(headX, headY, halo, 0, Math.PI * 2);
    context.fill();
    context.globalCompositeOperation = 'source-over';
  }

  function frame(now) {
    if (now - lastFrame < FRAME_INTERVAL) {
      frameId = window.requestAnimationFrame(frame);
      return;
    }
    frameId = 0;
    const elapsed = Math.min(0.05, (now - lastFrame) / 1000);
    lastFrame = now;
    context.clearRect(0, 0, width, height);
    context.drawImage(lattice, 0, 0, width, height);
    if (signals.length < MAX_SIGNALS && Math.random() < elapsed * 1.5) spawn();
    for (const signal of signals) {
      signal.age += elapsed;
      step(signal, SPEED * elapsed);
      drawSignal(signal, now);
    }
    signals = signals.filter((signal) => signal.age < signal.life);
    frameId = window.requestAnimationFrame(frame);
  }

  function rebuild() {
    if (frameId) window.cancelAnimationFrame(frameId);
    frameId = 0;
    dark = isDarkTheme();
    build();
    context.clearRect(0, 0, width, height);
    context.drawImage(lattice, 0, 0, width, height);
    if (reducedMotion) return;
    for (let index = 0; index < 5; index += 1) {
      spawn();
      signals[index].age = 1.2 + Math.random() * 3;
    }
    lastFrame = performance.now();
    frameId = window.requestAnimationFrame(frame);
  }

  const onSystemThemeChange = () => {
    if (root.dataset.theme === 'auto' && isDarkTheme() !== dark) rebuild();
  };
  const onMotionPreferenceChange = (event) => {
    reducedMotion = event.matches;
    rebuild();
  };
  rebuild();
  window.addEventListener('resize', rebuild, { passive: true });
  const themeObserver = new MutationObserver(() => {
    if (isDarkTheme() !== dark) rebuild();
  });
  themeObserver.observe(root, { attributes: true, attributeFilter: ['data-theme', 'class'] });
  systemTheme.addEventListener('change', onSystemThemeChange);
  motionPreference.addEventListener('change', onMotionPreferenceChange);
  activeHost = host;
  cleanup = () => {
    if (frameId) window.cancelAnimationFrame(frameId);
    frameId = 0;
    window.removeEventListener('resize', rebuild);
    systemTheme.removeEventListener('change', onSystemThemeChange);
    motionPreference.removeEventListener('change', onMotionPreferenceChange);
    themeObserver.disconnect();
    canvas.remove();
  };
  };
  document.addEventListener('DOMContentLoaded', start, { once: true });
  document.addEventListener('htmx:afterSettle', start);
  document.addEventListener('htmx:historyRestore', start);
  const detachObserver = new MutationObserver(() => {
    if (activeHost && (!activeHost.isConnected
      || !activeHost.contains(activeHost.querySelector('canvas.hex-comets-canvas')))) {
      stop();
    }
  });
  detachObserver.observe(document.documentElement, { childList: true, subtree: true });
  start();
})();
