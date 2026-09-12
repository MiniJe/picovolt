import pytest
from picovolt import Database,PicoVoltError,version

def test_retrieval_uses_native_engine_and_preserves_error_contract():
    assert version()=='2.2.0'
    with Database.open_memory() as db:
        db.query('CREATE TABLE docs(id,body,vector)')
        db.query('INSERT INTO docs VALUES(?,?,?)',[1,'Restore a verified backup','[1,0]'])
        request={'kind':'full_text','sql':'SELECT id,body FROM docs','id_column':'id','text_columns':['body'],'query':'verified backup','limit':5}
        assert db.retrieve(request)[0]['id']=='1'
        assert db.retrieve({'kind':'vector','sql':'SELECT id,vector FROM docs','id_column':'id','vector_column':'vector','query':[1,0],'metric':'cosine','limit':5})==[{'id':'1','distance':0.0}]
        with pytest.raises(PicoVoltError):db.retrieve({**request,'sql':'DELETE FROM docs WHERE id=1'})
        assert db.query('SELECT COUNT(*) FROM docs')['rows']==[[1]]
    with pytest.raises(PicoVoltError):db.retrieve(request)
