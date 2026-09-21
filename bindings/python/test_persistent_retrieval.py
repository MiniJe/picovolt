"""Named indexes use the maintained ctypes ABI, including reopen and rollback."""
import pytest
from picovolt import Database, PicoVoltError


def requests():
    common = {'sql': 'SELECT * FROM docs WHERE tenant=?', 'params': ['a'],
              'id_column': 'id', 'limit': 10}
    text = {**common, 'kind': 'full_text', 'text_columns': ['body'], 'query': 'apple'}
    vector = {**common, 'kind': 'vector', 'vector_column': 'embedding',
              'query': [1, 0], 'metric': 'cosine'}
    hybrid = {**text, 'kind': 'hybrid', 'vector_column': 'embedding',
              'vector_query': [1, 0], 'metric': 'cosine', 'text_weight': .5,
              'candidate_limit': 10}
    return [(text, {'index': 'ft'}), (vector, {'index': 'vx'}),
            (hybrid, {'text_index': 'ft', 'vector_index': 'vx'})]


def compare(db):
    for request, names in requests():
        expected = db.retrieve(request)
        assert expected and all(hit['id'] != '2' for hit in expected)
        assert db.retrieve({**request, **names}) == expected


def test_persistent_retrieval_reopen_commit_rollback_and_baked_image(tmp_path):
    root = str(tmp_path / 'workspace.pv')
    with Database.open_dev(root) as db:
        db.query('CREATE TABLE docs (id,tenant,body,embedding)')
        db.query("INSERT INTO docs VALUES (1,'a','apple apple','[1,0]'),(2,'b','apple','[0,1]')")
        db.query("CREATE INDEX ft ON docs USING FULLTEXT (body) WITH (id_column='id')")
        db.query("CREATE INDEX vx ON docs USING VECTOR (embedding) WITH (id_column='id',metric='cosine',dimensions=2)")
        compare(db)
        db.begin()
        db.query("UPDATE docs SET body='banana' WHERE id=1")
        db.query("UPDATE docs SET embedding='[0,1]' WHERE id=1")
        db.rollback()
        compare(db)
        db.query("INSERT INTO docs VALUES (3,'a','apple committed','[1,1]')")
    with Database.open_dev(root) as db:
        compare(db)
        image = db.export()
        assert int.from_bytes(image[4:6], 'little') == 8
    with Database.from_bytes(image) as db:
        compare(db)
        request, names = requests()[0]
        with pytest.raises(PicoVoltError):
            db.retrieve({**request, **names, 'text_columns': ['tenant']})
        db.query('DROP INDEX ft')
        with pytest.raises(PicoVoltError):
            db.retrieve({**request, **names})
    baked = tmp_path / 'image.pvdb'
    baked.write_bytes(image)
    with Database.open_prod(str(baked)) as db:
        compare(db)
