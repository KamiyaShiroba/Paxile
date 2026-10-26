// Evaluate in a real browser at 600, 500, 420 and 360 CSS pixels.
// Returns measured geometry; throws on off-centre, clipped or shrinking controls.
(() => {
  const rect = element => {
    const { x, y, width, height } = element.getBoundingClientRect();
    return { x, y, width, height };
  };
  const controls = ['minimize-settings', 'close-settings'].map(id => {
    const button = document.getElementById(id);
    const box = rect(button), icon = rect(button.querySelector('svg'));
    return { id, button: box, icon,
      dx: icon.x + icon.width / 2 - box.x - box.width / 2,
      dy: icon.y + icon.height / 2 - box.y - box.height / 2,
      shrink: getComputedStyle(button).flexShrink };
  });
  const errors = [];
  for (const control of controls) {
    if (Math.abs(control.dx) > 0.1 || Math.abs(control.dy) > 0.1)
      errors.push(`${control.id} is off-centre (${control.dx}, ${control.dy})`);
    if (control.icon.width !== 20 || control.icon.height !== 20)
      errors.push(`${control.id} icon is not 20px`);
    if (control.button.width !== control.button.height || control.shrink !== '0')
      errors.push(`${control.id} is not a nonshrinking square`);
    if (control.button.x + control.button.width > innerWidth)
      errors.push(`${control.id} is outside the viewport`);
  }
  const headerItems = [...document.querySelectorAll('.brand,.tabs,.top-actions')].map(rect);
  for (let i = 1; i < headerItems.length; i++)
    if (headerItems[i - 1].x + headerItems[i - 1].width > headerItems[i].x + 0.1)
      errors.push('header items overlap');
  for (const selector of ['#core-status', '#add-button', '.top-actions .toggle', '.tabs']) {
    const box = rect(document.querySelector(selector));
    if (box.width <= 0 || box.x < 0 || box.x + box.width > innerWidth)
      errors.push(`${selector} is not visible`);
  }
  const result = { width: innerWidth, controls, errors };
  if (errors.length) throw Error(JSON.stringify(result));
  return result;
})()
