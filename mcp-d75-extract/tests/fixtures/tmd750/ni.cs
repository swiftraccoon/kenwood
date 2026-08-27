public class ni
{
	private List<nj> c;

	public byte PmSelect
	{
		get { return 0; }
	}

	public string PmName1
	{
		get { return string.Empty; }
	}

	public void ai()
	{
		int num4 = 0;
		while (num4 < 6)
		{
			c.Add(new nj());
			c[num4].OffsetProgrammableMemoryAddress = 8192 * num4;
			num4++;
		}
	}

	public void a6(n7 A_0)
	{
		int num4 = 0;
		A_0.a(PmSelect, 323593);
		A_0.d(PmName1, 323594, oc.ba);
		while (num4 < 6)
		{
			c[num4].a6(A_0);
			num4++;
		}
	}

	public void a7(n7 A_0)
	{
		PmSelect = A_0.a(323593);
	}
}
