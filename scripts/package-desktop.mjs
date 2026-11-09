import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const args=process.argv.slice(2);
const skipBuild=args.includes('--skip-build');
const {version}=JSON.parse(await fs.readFile(path.join(root,'package.json'),'utf8'));
function run(command,argv,options={}){
  const result=spawnSync(command,argv,{cwd:root,stdio:'inherit',...options});
  if(result.error)throw result.error;
  if(result.status!==0)throw new Error(`${path.basename(command)} failed (${result.status})`);
}
if(process.platform!=='darwin'){
  run(process.execPath,[path.join(root,'node_modules/@tauri-apps/cli/tauri.js'),'build',...args.filter(a=>a!=='--skip-build')],{cwd:path.join(root,'apps/desktop')});
}else{
  // Build the app through Tauri; make the DMG without Finder Apple Events.
  // This also works on interactive machines that deny Finder automation.
  const forwarded=[];
  for(let i=0;i<args.length;i++){
    if(args[i]==='--skip-build')continue;
    if(args[i]==='--bundles'){i++;continue;}
    if(args[i].startsWith('--bundles='))continue;
    forwarded.push(args[i]);
  }
  if(forwarded.includes('--target'))throw new Error('Package on the native macOS architecture so worker and app architectures match.');
  if(!skipBuild)run(process.execPath,[path.join(root,'node_modules/@tauri-apps/cli/tauri.js'),'build','--bundles','app',...forwarded],{cwd:path.join(root,'apps/desktop')});
  const target=process.env.CARGO_TARGET_DIR?path.resolve(root,process.env.CARGO_TARGET_DIR):path.join(root,'target');
  const app=path.join(target,'release/bundle/macos/Gemma Harness.app');
  await fs.access(app);
  run('/usr/bin/codesign',['--force','--deep','--sign','-',app]);
  run('/usr/bin/codesign',['--verify','--deep','--strict',app]);
  const staging=await fs.mkdtemp(path.join(os.tmpdir(),'gemma-harness-dmg-'));
  const output=path.join(target,'release/bundle/dmg',`Gemma Harness_${version}_${process.arch==='arm64'?'aarch64':'x64'}.dmg`);
  try{
    await fs.cp(app,path.join(staging,'Gemma Harness.app'),{recursive:true,verbatimSymlinks:true});
    await fs.symlink('/Applications',path.join(staging,'Applications'));
    await fs.mkdir(path.dirname(output),{recursive:true});
    run('/usr/bin/hdiutil',['create','-volname','Gemma Harness','-srcfolder',staging,'-ov','-format','UDZO',output]);
    run('/usr/bin/hdiutil',['verify',output]);
    process.stdout.write(`\nBuilt and verified local ad-hoc signed app + DMG:\n${app}\n${output}\n`);
  }finally{await fs.rm(staging,{recursive:true,force:true});}
}
