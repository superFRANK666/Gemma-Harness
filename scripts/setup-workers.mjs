import fs from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const manifest=JSON.parse(await fs.readFile(path.join(root,'assets/node-runtime.json'),'utf8'));
const platform=`${process.platform}-${process.arch}`;
const asset=manifest.assets[platform];
if(!asset) throw new Error(`No pinned Node runtime for ${platform}`);
const resourceRoot=path.join(root,'build/worker-resources');
const staging=path.join(resourceRoot,'workers');
const cache=path.join(root,'build/downloads',asset.file);
await fs.mkdir(resourceRoot,{recursive:true});
await fs.mkdir(path.dirname(cache),{recursive:true});
const temporary=await fs.mkdtemp(path.join(os.tmpdir(),'harness-node-'));
const prepared=await fs.mkdtemp(path.join(resourceRoot,'.workers-staging-'));
async function sha256(file){
  const hash=createHash('sha256');
  for await(const chunk of createReadStream(file))hash.update(chunk);
  return hash.digest('hex');
}
try {
  const url=`https://nodejs.org/dist/${manifest.version}/${asset.file}`;
  let cached=false;
  try{cached=(await sha256(cache))===asset.sha256;}catch(error){if(error.code!=='ENOENT')throw error;}
  if(!cached){
    process.stdout.write(`Downloading verified browser runtime ${manifest.version} (${platform})…\n`);
    const response=await fetch(url);if(!response.ok)throw new Error(`Node download HTTP ${response.status}`);
    const data=Buffer.from(await response.arrayBuffer());
    if(createHash('sha256').update(data).digest('hex')!==asset.sha256)throw new Error('Node distribution SHA256 mismatch');
    await fs.writeFile(path.join(temporary,asset.file),data);
    await fs.copyFile(path.join(temporary,asset.file),cache);
  }else process.stdout.write(`Using verified cached browser runtime ${manifest.version} (${platform}).\n`);
  const archive=cache;
  execFileSync('tar',['-xf',archive,'-C',temporary]);
  const source=path.join(temporary,asset.file.replace(/\.(tar\.gz|zip)$/,''));
  const nodeName=process.platform==='win32'?'node.exe':'node';
  const binary=path.join(source,process.platform==='win32'?'node.exe':'bin/node');
  for(const destination of [path.join(root,'workers/browser/bin',nodeName),path.join(prepared,'browser/bin',nodeName)]){
    await fs.mkdir(path.dirname(destination),{recursive:true});await fs.copyFile(binary,destination);await fs.chmod(destination,0o755);
  }
  await fs.copyFile(path.join(source,'LICENSE'),path.join(prepared,'browser/NODE-LICENSE'));
  await fs.copyFile(path.join(root,'workers/browser/worker.mjs'),path.join(prepared,'browser/worker.mjs'));
  for(const name of ['playwright','playwright-core']){
    await fs.cp(path.join(root,'workers/browser/node_modules',name),path.join(prepared,'browser/node_modules',name),{recursive:true,dereference:true});
  }
  const nativePlatform={darwin:'macos',win32:'windows',linux:'linux'}[process.platform];
  const helperName=process.platform==='win32'?'harness-desktop.exe':'harness-desktop';
  await fs.mkdir(path.join(prepared,'native/bin',nativePlatform),{recursive:true});
  await fs.copyFile(path.join(root,'workers/native/bin',nativePlatform,helperName),path.join(prepared,'native/bin',nativePlatform,helperName));
  await fs.chmod(path.join(prepared,'native/bin',nativePlatform,helperName),0o755);
  const backup=await fs.mkdtemp(path.join(resourceRoot,'.workers-backup-'));
  await fs.rmdir(backup);
  let hadPrevious=false;
  try{await fs.rename(staging,backup);hadPrevious=true;}catch(error){if(error.code!=='ENOENT')throw error;}
  try{await fs.rename(prepared,staging);}catch(error){
    if(hadPrevious){
      try{await fs.rename(backup,staging);}catch(restoreError){throw new AggregateError([error,restoreError],`Could not restore previous resources at ${backup}`);}
    }
    throw error;
  }
  if(hadPrevious)await fs.rm(backup,{recursive:true,force:true});
  process.stdout.write(`Worker resources staged in ${path.relative(root,staging)}\n`);
}finally{await fs.rm(temporary,{recursive:true,force:true});await fs.rm(prepared,{recursive:true,force:true});}
