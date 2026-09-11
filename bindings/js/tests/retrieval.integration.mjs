import assert from 'node:assert/strict';
import {test} from 'node:test';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const directory=resolve(process.env.PICOVOLT_JS_PACKAGE_DIR||'artifacts/npm-2.1.0');
test('2.1 WASM and JS adapter execute native full-text and vector retrieval',async()=>{
  const {default:Database}=await import(pathToFileURL(resolve(directory,'sqlite.js')));
  const db=new Database();db.exec('CREATE TABLE docs(id,body,embedding)');
  db.prepare('INSERT INTO docs VALUES(?,?,?)').run(1,'Verified backup restore','[1,0]');
  const text={kind:'full_text',sql:'SELECT id,body FROM docs',id_column:'id',text_columns:['body'],query:'backup restore',limit:5};
  assert.equal(db.retrieve(text)[0].id,'1');
  assert.deepEqual(db.retrieve({kind:'vector',sql:'SELECT id,embedding FROM docs',id_column:'id',vector_column:'embedding',query:[1,0],metric:'cosine',limit:5}),[{id:'1',distance:0}]);
  assert.throws(()=>db.retrieve({...text,sql:'DELETE FROM docs WHERE id=1'}));
  assert.equal(db.prepare('SELECT COUNT(*) AS count FROM docs').get().count,1);
  db.close();assert.throws(()=>db.retrieve(text),/closed/);
});
