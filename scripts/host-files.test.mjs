import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
const script = readFileSync(new URL('../shell/index.html', import.meta.url), 'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
test('in-place save keeps the explicit renamed path despite a conflicting name map', async () => {
  const writes = []; let picks = 0;
  const window = { localStorage: { getItem: () => JSON.stringify({'renamed.pdf':'/other/renamed.pdf'}), setItem() {} },
    otools: { dialog: { open: async () => null, save: async () => { picks++; return '/wrong/renamed.pdf'; } },
      readHostFile: async () => ({}), writeHostFile: async (...args) => { writes.push(args); } } };
  vm.runInNewContext(script, { window, console });
  const result = await window.pdfcraftOtools.saveFile({name:'renamed.pdf', path:'/chosen/renamed.pdf', dataBase64:'UERG', forcePick:false});
  assert.equal(result.path, '/chosen/renamed.pdf');
  assert.equal(picks, 0);
  assert.equal(writes.length, 1);
  assert.ok(JSON.stringify(writes[0]).includes('/chosen/renamed.pdf'));
});
