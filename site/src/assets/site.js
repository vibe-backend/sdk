// Theme toggle, language memory, mobile menu and scroll reveal. No dependencies.
(function () {
  var doc = document.documentElement;
  var store = function (k, v) { try { localStorage.setItem(k, v); } catch (e) { /* private mode */ } };

  var theme = document.querySelector('.theme');
  if (theme) theme.addEventListener('click', function () {
    var cur = doc.dataset.theme || (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
    var next = cur === 'dark' ? 'light' : 'dark';
    doc.dataset.theme = next; store('vb-theme', next);
  });

  document.querySelectorAll('[data-lang]').forEach(function (a) {
    var base = a.href.split('#')[0];
    a.addEventListener('click', function () { store('vb-lang', a.getAttribute('data-lang')); a.href = base + location.hash; });
  });

  var menu = document.querySelector('.menu');
  if (menu) menu.addEventListener('click', function () {
    var open = menu.getAttribute('aria-expanded') !== 'true';
    menu.setAttribute('aria-expanded', String(open));
    document.body.classList.toggle('nav-open', open);
  });

  var items = document.querySelectorAll('.reveal');
  if (!('IntersectionObserver' in window) || matchMedia('(prefers-reduced-motion: reduce)').matches) {
    items.forEach(function (el) { el.classList.add('in'); });
    return;
  }
  var io = new IntersectionObserver(function (entries) {
    entries.forEach(function (e) { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } });
  }, { rootMargin: '0px 0px -8% 0px', threshold: 0.08 });
  items.forEach(function (el) { io.observe(el); });
})();
