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

test('batch export picks the folder once and writes every page under its own name', async () => {
  const writes = []; let folderPicks = 0, savePicks = 0;
  const store = {};
  const window = {
    localStorage: { getItem: (k) => store[k] ?? null, setItem: (k, v) => { store[k] = v; } },
    otools: {
      dialog: {
        open: async (o) => { folderPicks++; assert.equal(o.directory, true); return '/out/export'; },
        save: async () => { savePicks++; return '/should/not/be/used.pdf'; },
      },
      readHostFile: async () => ({}),
      writeHostFile: async (arg) => { writes.push(arg); },
    },
  };
  vm.runInNewContext(script, { window, console, Promise });
  // Same tick: 3 pages, all forcePick (as export does).
  const results = await Promise.all([
    window.pdfcraftOtools.saveFile({name:'doc_page_1.png', dataBase64:'MQ==', forcePick:true}),
    window.pdfcraftOtools.saveFile({name:'doc_page_2.png', dataBase64:'Mg==', forcePick:true}),
    window.pdfcraftOtools.saveFile({name:'doc_page_3.png', dataBase64:'Mw==', forcePick:true}),
  ]);
  assert.equal(folderPicks, 1, 'folder picker must open exactly once');
  assert.equal(savePicks, 0, 'no per-page save dialog');
  assert.equal(writes.length, 3, 'every page is written');
  assert.deepEqual(writes.map((w) => w.path).sort(),
    ['/out/export/doc_page_1.png', '/out/export/doc_page_2.png', '/out/export/doc_page_3.png']);
  assert.deepEqual(results.map((r) => r.path).sort(),
    ['/out/export/doc_page_1.png', '/out/export/doc_page_2.png', '/out/export/doc_page_3.png']);
});

test('cancelling the folder picker resolves every batched save as cancelled', async () => {
  const writes = []; let folderPicks = 0;
  const window = {
    localStorage: { getItem: () => null, setItem() {} },
    otools: {
      dialog: {
        open: async () => { folderPicks++; return null; },
        save: async () => '/unused',
      },
      readHostFile: async () => ({}),
      writeHostFile: async (path) => { writes.push(path); },
    },
  };
  vm.runInNewContext(script, { window, console, Promise });
  const results = await Promise.all([
    window.pdfcraftOtools.saveFile({name:'a_page_1.png', dataBase64:'MQ==', forcePick:true}),
    window.pdfcraftOtools.saveFile({name:'a_page_2.png', dataBase64:'Mg==', forcePick:true}),
  ]);
  assert.equal(folderPicks, 1);
  assert.equal(writes.length, 0, 'nothing written when cancelled');
  assert.deepEqual(results, [null, null], 'each page reports cancellation');
});

test('single forcePick save still uses the save dialog (unchanged behavior)', async () => {
  const writes = []; let savePicks = 0, folderPicks = 0;
  const window = {
    localStorage: { getItem: () => null, setItem() {} },
    otools: {
      dialog: {
        open: async () => { folderPicks++; return '/out'; },
        save: async () => { savePicks++; return '/chosen/one.pdf'; },
      },
      readHostFile: async () => ({}),
      writeHostFile: async (path) => { writes.push(path); },
    },
  };
  vm.runInNewContext(script, { window, console, Promise });
  const result = await window.pdfcraftOtools.saveFile({name:'one.pdf', dataBase64:'UERG', forcePick:true});
  assert.equal(savePicks, 1);
  assert.equal(folderPicks, 0);
  assert.equal(writes.length, 1);
  assert.equal(result.path, '/chosen/one.pdf');
});
