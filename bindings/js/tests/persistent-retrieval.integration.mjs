import assert from 'node:assert/strict';
import {test} from 'node:test';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

const directory = resolve(process.env.PICOVOLT_JS_PACKAGE_DIR || 'pkg');

test('named persistent retrieval survives WASM export/import and adapter rollback', async () => {
  const {default: Database} = await import(pathToFileURL(resolve(directory, 'sqlite.js')));
  const {Db} = await import(pathToFileURL(resolve(directory, 'picovolt.js')));
  const db = new Database();
  let reopened;
  try {
    db.exec('CREATE TABLE docs (id,tenant,body,embedding)');
    db.exec("INSERT INTO docs VALUES (1,'a','apple apple','[1,0]'),(2,'b','apple','[0,1]')");
    db.exec("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')");
    db.exec("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)");
    const text = {kind:'full_text',sql:"SELECT * FROM docs WHERE tenant='a'",id_column:'id',text_columns:['body'],query:'apple',limit:10};
    const vector = {kind:'vector',sql:text.sql,id_column:'id',vector_column:'embedding',query:[1,0],metric:'cosine',limit:10};
    const hybrid = {...text,kind:'hybrid',vector_column:'embedding',vector_query:[1,0],metric:'cosine',text_weight:0.5,candidate_limit:10};
    const pairs = [[text,{index:'ft'}],[vector,{index:'vx'}],[hybrid,{text_index:'ft',vector_index:'vx'}]];
    for (const [request,names] of pairs) {
      const expected = db.retrieve(request);
      assert.equal(expected[0].id,'1');
      assert.deepEqual(db.retrieve({...request,...names}),expected);
    }
    assert.throws(db.transaction(() => {
      db.exec("UPDATE docs SET embedding='[0,1]' WHERE id=1");
      db.exec("UPDATE docs SET body='banana' WHERE id=1");
      throw new Error('abort test transaction');
    }), /abort test transaction/);
    const image = db.serialize();
    assert.equal(new DataView(image.buffer, image.byteOffset, image.byteLength).getUint16(4,true),8);
    reopened = Db.fromBytes(image);
    for (const [request,names] of pairs) {
      assert.deepEqual(JSON.parse(reopened.retrieve(JSON.stringify({...request,...names}))),db.retrieve(request));
    }
    assert.throws(() => db.retrieve({...text,index:'vx'}));
  } finally {
    reopened?.free();
    db.close();
  }
});
