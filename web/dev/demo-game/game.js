// Ember Drift: a tiny canvas game used to demonstrate Vesper web builds.
(() => {
  const canvas = document.getElementById("game");
  const ctx = canvas.getContext("2d");
  const score = document.getElementById("score");
  const keys = new Set();
  addEventListener("keydown", (e) => keys.add(e.key.toLowerCase()));
  addEventListener("keyup", (e) => keys.delete(e.key.toLowerCase()));
  const resize = () => { canvas.width = innerWidth; canvas.height = innerHeight; };
  addEventListener("resize", resize); resize();
  const player = { x: innerWidth / 2, y: innerHeight / 2, vx: 0, vy: 0, r: 10 };
  let embers = [], shadows = [], points = 0, last = performance.now(), dead = 0;
  const rand = (a, b) => a + Math.random() * (b - a);
  const spawn = (list, n, speed) => { for (let i = 0; i < n; i++) list.push({ x: rand(0, canvas.width), y: rand(0, canvas.height), vx: rand(-speed, speed), vy: rand(-speed, speed), r: rand(5, 9) }); };
  const reset = () => { embers = []; shadows = []; points = 0; spawn(embers, 12, 20); spawn(shadows, 4, 60); player.x = canvas.width / 2; player.y = canvas.height / 2; };
  reset();
  function step(now) {
    const dt = Math.min(0.05, (now - last) / 1000); last = now;
    const ax = (keys.has("arrowright") || keys.has("d")) - (keys.has("arrowleft") || keys.has("a"));
    const ay = (keys.has("arrowdown") || keys.has("s")) - (keys.has("arrowup") || keys.has("w"));
    player.vx = (player.vx + ax * 900 * dt) * 0.92; player.vy = (player.vy + ay * 900 * dt) * 0.92;
    player.x = Math.max(0, Math.min(canvas.width, player.x + player.vx * dt));
    player.y = Math.max(0, Math.min(canvas.height, player.y + player.vy * dt));
    for (const o of [...embers, ...shadows]) {
      o.x += o.vx * dt; o.y += o.vy * dt;
      if (o.x < 0 || o.x > canvas.width) o.vx *= -1;
      if (o.y < 0 || o.y > canvas.height) o.vy *= -1;
    }
    for (const s of shadows) { s.r += dt * 1.2; if (Math.hypot(s.x - player.x, s.y - player.y) < s.r + player.r) { dead = 1.2; reset(); } }
    embers = embers.filter((e) => { const hit = Math.hypot(e.x - player.x, e.y - player.y) < e.r + player.r + 4; if (hit) points++; return !hit; });
    if (embers.length < 6) { spawn(embers, 6, 20 + points); spawn(shadows, 1, 60 + points * 2); }
    score.textContent = points;
    ctx.fillStyle = dead > 0 ? `rgba(60,10,10,${0.3 + dead / 3})` : "rgba(7,9,13,0.35)"; dead = Math.max(0, dead - dt);
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    for (const e of embers) { ctx.fillStyle = "#ffb347"; ctx.shadowColor = "#ff8a1f"; ctx.shadowBlur = 18; ctx.beginPath(); ctx.arc(e.x, e.y, e.r, 0, 7); ctx.fill(); }
    ctx.shadowBlur = 0;
    for (const s of shadows) { ctx.fillStyle = "#1d1430"; ctx.strokeStyle = "#5b3fa0"; ctx.beginPath(); ctx.arc(s.x, s.y, s.r, 0, 7); ctx.fill(); ctx.stroke(); }
    ctx.fillStyle = "#b3f264"; ctx.shadowColor = "#b3f264"; ctx.shadowBlur = 22; ctx.beginPath(); ctx.arc(player.x, player.y, player.r, 0, 7); ctx.fill(); ctx.shadowBlur = 0;
    requestAnimationFrame(step);
  }
  requestAnimationFrame(step);
})();
