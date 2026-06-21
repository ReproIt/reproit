// A delegated-click SPA: a multi-view "Settings" app navigated ENTIRELY by
// clickable <div role="option" tabindex="-1" data-testid="..."> rows.
//
// There is NO native <button>/<a> anywhere. Every control is a non-interactive
// element (a <div>) that becomes operable only because of ONE document-level
// delegated click listener (see the bottom of this file). cursor:pointer + the
// ARIA-interactive role `option` + tabindex=-1 are the markers that tell a
// pointer user (and reproit's coverage fix) "this is operable".
//
// BEFORE the runner.mjs coverage fix, the crawler tapped only elements that
// match its interactive() grammar (native controls, onclick attr, tabindex>=0).
// tabindex=-1 delegated <div>s match NONE of those, so the explorer never tapped
// them -> the whole app mapped to ~1 state / 0 transitions and 0 of the bugs
// below were reachable. The fix adds KEYED pointer-operable controls (this exact
// pattern) to the candidate set, so the explorer now navigates the app.
//
// ===========================================================================
// SEEDED BUGS (each deterministic; documented precisely in the README)
// ===========================================================================
//   BUG 1 (crash / no-exception): opening the "Danger zone" view, then tapping
//          its "Delete account" row, calls account.purge() but `account` has no
//          purge method -> uncaught TypeError on every tap. Caught by the crash
//          oracle (pageerror).
//   BUG 2 (crash / no-exception, DISTINCT signature): in the Profile view,
//          tapping "Save profile" reads form.serialize() where `form` is null
//          -> a DIFFERENT uncaught TypeError ("Cannot read properties of null").
//   BUG 3 (dead-end / no-dead-end): the "Appearance" view renders NO back row
//          and NO nav rows -> a state with no outgoing action edge, escapable
//          only via system back. Caught by the graph dead-end oracle.
//   BUG 4 (operability / all-labeled): the Notifications view has an icon-only
//          control (a decorative aria-hidden glyph, no text, no aria-label) that
//          is pointer-operable but has NO accessible name -> an unlabeled
//          tappable. Caught by the all-labeled (operability/a11y) oracle.
//   BUG 5 (i18n / locale-specific crash): the "About" row's handler throws ONLY
//          when navigator.language starts with "de" (a German-only code path:
//          it calls intl.formatDE() which is undefined). Under default/en it is
//          harmless; under --locale de it crashes -> surfaces in the cross-locale
//          i18n diff as a locale-specific finding.
// ===========================================================================

// --- intentionally incomplete objects, to make the bugs deterministic -------
const account = {
  name: 'alice',
  // NOTE: no purge() method -> BUG 1 crashes here.
};
const intl = {
  formatEN(s) { return s; },
  // NOTE: no formatDE() method -> BUG 5 crashes under German locale.
};

const titleEl = () => document.getElementById('title');
const viewEl = () => document.getElementById('view');

// --- tiny view helpers ------------------------------------------------------
// A navigable delegated row: a <div role=option tabindex=-1 data-testid=...>.
function row(testid, label, opts = {}) {
  const chevron = opts.chevron === false ? '' : '<span class="chevron">&rsaquo;</span>';
  const cls = opts.danger ? 'row danger' : 'row';
  return `<div class="${cls}" role="option" tabindex="-1" data-testid="${testid}">
    <span>${label}</span>${chevron}
  </div>`;
}
function backRow(testid) {
  return `<div class="back" role="option" tabindex="-1" data-testid="${testid}">&lsaquo; Back</div>`;
}

// --- the views --------------------------------------------------------------
const VIEWS = {
  home() {
    titleEl().textContent = 'Settings';
    return `
      ${row('nav-profile', 'Profile')}
      ${row('nav-notifications', 'Notifications')}
      ${row('nav-appearance', 'Appearance')}
      ${row('nav-about', 'About')}
      ${row('nav-danger', 'Danger zone', { danger: true })}
    `;
  },

  profile() {
    titleEl().textContent = 'Profile';
    return `
      ${backRow('profile-back')}
      <h2>Profile</h2>
      <p class="muted">Signed in as ${account.name}.</p>
      ${row('profile-edit', 'Edit display name')}
      ${row('profile-save', 'Save profile', { chevron: false })}
      ${row('profile-to-notifications', 'Manage notifications')}
    `;
  },

  notifications() {
    titleEl().textContent = 'Notifications';
    return `
      ${backRow('notifications-back')}
      <h2>Notifications</h2>
      ${row('notif-email', 'Email alerts')}
      ${row('notif-push', 'Push alerts')}
      ${row('notif-to-profile', 'Profile settings')}
      <div style="display:flex; align-items:center; justify-content:space-between; margin-top:0.75rem;">
        <span class="muted">Sound profile</span>
        <!-- BUG 4: pointer-operable, but the icon is a CSS ::before glyph (no DOM
             text node) and there is no aria-label/title -> NO accessible name
             (an unlabeled tappable). -->
        <div class="iconbtn" role="option" tabindex="-1" data-testid="notif-sound"></div>
      </div>
    `;
  },

  // BUG 3: NO back row, NO nav rows -> a dead end (no outgoing action edge).
  appearance() {
    titleEl().textContent = 'Appearance';
    return `
      <h2>Appearance</h2>
      <p class="muted">Theme follows the system setting.</p>
      <p class="muted">There is no way back from here from the UI.</p>
    `;
  },

  about() {
    titleEl().textContent = 'About';
    return `
      ${backRow('about-back')}
      <h2>About</h2>
      <p class="muted">Settings demo, version 1.0.</p>
      ${row('about-credits', 'Credits')}
      ${row('about-to-profile', 'Account')}
    `;
  },

  danger() {
    titleEl().textContent = 'Danger zone';
    return `
      ${backRow('danger-back')}
      <h2 class="danger">Danger zone</h2>
      <p class="muted">Irreversible actions.</p>
      ${row('danger-delete', 'Delete account', { danger: true, chevron: false })}
      ${row('danger-to-profile', 'Back to profile')}
    `;
  },
};

function render(name) {
  viewEl().dataset.view = name;
  viewEl().innerHTML = (VIEWS[name] || VIEWS.home)();
}

// --- the SINGLE document-level delegated click listener ---------------------
// This is what makes every <div role=option tabindex=-1> operable. No control
// has its own listener; the whole app is one delegated handler keyed by
// data-testid. This is the exact pattern the coverage fix targets.
document.addEventListener('click', (ev) => {
  const el = ev.target.closest('[data-testid]');
  if (!el) return;
  const id = el.getAttribute('data-testid');

  switch (id) {
    // ---- navigation ----
    case 'nav-profile':       return render('profile');
    case 'nav-notifications': return render('notifications');
    case 'nav-appearance':    return render('appearance');
    case 'nav-danger':        return render('danger');

    // ---- BUG 5: locale-specific crash (only under German) ----
    // Navigating to About runs a locale-sensitive formatter. Under en it is
    // harmless; under de it calls intl.formatDE() which is undefined -> an
    // uncaught TypeError reachable simply by tapping "About". Locale-specific,
    // so it surfaces in the cross-locale i18n diff.
    case 'nav-about': {
      render('about');
      const lang = (navigator.language || '').toLowerCase();
      if (lang.startsWith('de')) {
        intl.formatDE('Über'); // throws under German locale only
      } else {
        intl.formatEN('About');
      }
      return;
    }

    // ---- cross-navigation (real outgoing ACTION edges between views) ----
    // These give Profile/Notifications/About an outgoing non-back action edge,
    // so the ONLY view with no outgoing action edge is Appearance (BUG 3). That
    // makes the dead-end finding land squarely on the Appearance state.
    case 'profile-to-notifications': return render('notifications');
    case 'notif-to-profile':         return render('profile');
    case 'about-to-profile':         return render('profile');
    case 'danger-to-profile':        return render('profile');

    // ---- back rows ----
    case 'profile-back':
    case 'notifications-back':
    case 'about-back':
    case 'danger-back':
      return render('home');

    // ---- harmless leaf actions (no state change, but not crashes) ----
    case 'profile-edit':  return; // would open an inline editor; no-op here
    case 'notif-email':   return;
    case 'notif-push':    return;
    case 'notif-sound':   return; // the unlabeled control: operable, does nothing visible
    case 'about-credits': return;

    // ---- BUG 2: distinct uncaught TypeError (null deref) ----
    case 'profile-save': {
      const form = document.querySelector('#nonexistent-form'); // null
      form.serialize(); // throws: Cannot read properties of null
      return;
    }

    // ---- BUG 1: uncaught TypeError, account has no purge() ----
    case 'danger-delete':
      account.purge(); // throws: account.purge is not a function
      return;
  }
});

render('home');
