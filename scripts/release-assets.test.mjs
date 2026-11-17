import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync,readFileSync,rmSync,writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { platformFiles,prepareRelease,releaseRepository } from './release-assets.mjs';

const info={build:51,version:'0.0.51',commit:'a'.repeat(40),tag:'build-51'};
function fixture(run) {
  const directory=mkdtempSync(join(tmpdir(),'auram-publish-'));
  const metadata={};
  try {
    for(const [platform,{updater,installer}] of Object.entries(platformFiles(info))) {
      const names=[...new Set([updater,`${updater}.sig`,installer])];
      const signature=Buffer.from('fixture signature').toString('base64');
      metadata[platform]={...info,platform,url:`https://github.com/${releaseRepository}/releases/download/${info.tag}/${updater}`,signature,
        files:names.map(name=>{
          const bytes=Buffer.from(name.endsWith('.sig')?signature:`fixture body: ${name}`);
          writeFileSync(join(directory,name),bytes);
          return {name,size:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')};
        })};
    }
    const save=()=>Object.entries(metadata).forEach(([platform,value])=>writeFileSync(join(directory,`${platform}.json`),JSON.stringify(value)));
    save();run({directory,metadata,save});
  } finally {rmSync(directory,{recursive:true,force:true});}
}
test('complete platform bodies produce immutable updater URLs and matching checksums',()=>fixture(({directory})=>{
  const result=prepareRelease(directory,info);
  const latest=JSON.parse(readFileSync(join(directory,'latest.json'),'utf8'));
  assert.equal(latest.version,info.version);
  assert.deepEqual(Object.keys(latest.platforms),['darwin-aarch64','windows-x86_64']);
  assert.equal(result.files.length,10);
  assert.deepEqual(JSON.parse(readFileSync(join(directory,'build.json'),'utf8')),info);
  for(const line of readFileSync(join(directory,'SHA256SUMS'),'utf8').trim().split('\n')) {
    const [hash,name]=line.split('  ');
    assert.equal(hash,createHash('sha256').update(readFileSync(join(directory,name))).digest('hex'));
  }
}));
test('a missing Mac installer cannot publish an otherwise valid updater',()=>fixture(({directory,metadata,save})=>{
  metadata['darwin-aarch64'].files=metadata['darwin-aarch64'].files.filter(file=>!file.name.endsWith('.dmg'));save();
  assert.throws(()=>prepareRelease(directory,info),/Incomplete distribution/);
}));
test('mismatched commits and corrupt transferred artifacts are rejected',()=>fixture(({directory,metadata,save})=>{
  metadata['windows-x86_64'].commit='b'.repeat(40);save();
  assert.throws(()=>prepareRelease(directory,info),/Mismatched build/);
  metadata['windows-x86_64'].commit=info.commit;save();
  writeFileSync(join(directory,'Auram.app.tar.gz'),'corrupt');
  assert.throws(()=>prepareRelease(directory,info),/Artifact integrity mismatch/);
}));
test('mutable URLs, foreign filenames and mismatched updater signatures are rejected',()=>fixture(({directory,metadata,save})=>{
  const mac=metadata['darwin-aarch64'];const url=mac.url;
  mac.url=url.replace('build-51','latest');save();
  assert.throws(()=>prepareRelease(directory,info),/immutable release/);
  mac.url=url;const filename=mac.files[0].name;mac.files[0].name='../foreign';save();
  assert.throws(()=>prepareRelease(directory,info),/Incomplete distribution/);
  mac.files[0].name=filename;mac.signature='different';save();
  assert.throws(()=>prepareRelease(directory,info),/Updater signature mismatch/);
}));
