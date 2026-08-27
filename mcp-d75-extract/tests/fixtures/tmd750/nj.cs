public class nj
{
	private int a;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			a = value;
		}
	}

	public bool PmAutoStore
	{
		get { return false; }
	}

	public void a6(n7 A_0)
	{
		A_0.a(PmAutoStore, 332824 + a);
	}

	public void a7(n7 A_0)
	{
		PmAutoStore = A_0.a(332824 + a) != 0;
	}
}
